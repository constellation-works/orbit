//! Owner-side fulfilment of a review evidence hold whose every requirement is
//! a Linux CodeQL run or a Linux `host_sandbox_test`.
//!
//! A non-Linux reviewer cannot complete `scripts/codeql-rust-local.sh` (it
//! exits 3), so it holds the review for a `codeql` result instead. A reviewer
//! inside a Linux agent lane cannot run a test of Orbit's Bubblewrap paths:
//! the lane's own sandbox refuses a nested one, so the test defers. It holds
//! the review for a `host_sandbox_test` result on `linux` [ORB-14334]. On a
//! Linux owner, each clock tick finds those holds and dispatches
//! `review_evidence_fulfilment_pipeline`, whose one step runs each named
//! command at the held commit and attaches the result and its log. Receipt of
//! every matching result then queues the task for a fresh review through
//! [`super::evidence::resume_evidence_hold`]; a fulfilment never approves a
//! candidate.
//!
//! A hold is fulfilled only when:
//!
//! - it is still the in-progress task's latest decision
//!   ([`super::evidence::hold_is_current`]) and its evidence has not arrived;
//! - every command is admitted before anything runs: a `codeql` requirement
//!   names the local CodeQL script with nothing but its own options and one
//!   query selector; a `host_sandbox_test` is
//!   `cargo test -p <crate> --test <target> [<filter>]` or an exact owner
//!   `workflow.required_validation_commands` entry
//!   ([`HostSandboxCommand::admit`]). So a hold can never make the owner run
//!   another command;
//! - the host is Linux with working Bubblewrap namespaces, and the filesystem
//!   holding the scratch checkout has at least the run's `min_free_mib` free;
//! - the held commit, fetched from `origin` when the owner lacks it, has the
//!   held tree.
//!
//! A CodeQL run executes without a shell in a standalone shallow checkout,
//! confined by Bubblewrap to that checkout. A host sandbox test cannot run
//! under that confinement: on a host that restricts unprivileged user
//! namespaces, AppArmor runs every Bubblewrap child under a profile that
//! denies the capabilities a nested Bubblewrap needs, so the test would defer
//! exactly as it did in the agent lane. Landlock likewise forbids the mounts
//! Bubblewrap makes. It therefore runs like the owner's required validation,
//! under the same host trust: in a fresh detached worktree of the held
//! commit, with the validation environment and a run-owned Cargo target
//! directory, removed afterwards ([`run_host_sandbox_test`]).
//!
//! A CodeQL run that exits nonzero, reports incomplete extraction, times out,
//! leaves no complete SARIF, or reports any result attaches its log but no
//! result. A host test run that fails, defers or skips itself, runs no test,
//! or meets an unavailable sandbox does too. Either way the hold stays in
//! place with a typed reason. Every attempt is audited under
//! [`EVIDENCE_FULFILMENT_AUDIT`] and commented on the task. At most
//! [`MAX_ACTIVE_EVIDENCE_FULFILMENTS`] run at once; a hold gets one run
//! unless a run refused it for a reason that can clear (disk, fetch) or ended
//! without an outcome, up to [`MAX_FULFILMENT_ATTEMPTS`].

mod codeql;
mod fulfil;
mod refusal;
mod tick;

pub(crate) use fulfil::fulfil_review_evidence;
pub(super) use fulfil::fulfilment_artifact_paths;
pub(crate) use refusal::FulfilmentRefusal;
pub use tick::EvidenceFulfilmentTick;

use orbit_types::workflow::ReviewEvidenceRequirement;
use serde_json::{Map, Value};

/// The job each tick dispatches, one run per fulfilment attempt.
pub const REVIEW_EVIDENCE_FULFILMENT_JOB: &str = "review_evidence_fulfilment_pipeline";
/// Audit command name every fulfilment decision is recorded under.
pub const EVIDENCE_FULFILMENT_AUDIT: &str = "review.evidence_fulfilment";
/// Fulfilment runs live at once; a CodeQL run or a test build is heavy on
/// memory and disk.
pub(crate) const MAX_ACTIVE_EVIDENCE_FULFILMENTS: usize = 1;
/// Runs one hold may have: the first, plus reruns after a refusal that can
/// clear without a decision (not enough disk, an unreachable candidate).
pub(crate) const MAX_FULFILMENT_ATTEMPTS: usize = 3;
/// Free space the state directory's filesystem needs before a run starts,
/// when neither the job's `default_input` nor the run input names one: a
/// CodeQL database, toolchain and Cargo build, or a test target's build.
pub(crate) const DEFAULT_FULFILMENT_MIN_FREE_MIB: u64 = 30 * 1024;
/// Run-input field naming the hold; every attempt for it shares the value.
const HOLD_KEY_FIELD: &str = "hold_key";
/// Run-input field the keyed admission matches: the hold key and attempt.
const ATTEMPT_KEY_FIELD: &str = "attempt_key";
/// Newest fulfilment runs the tick and keyed admission look through.
const RUN_SCAN_LIMIT: usize = 200;
/// The fulfilment step's id in the shipped job.
const FULFIL_STEP: &str = "fulfil";
const TRIGGER_NAME: &str = "review-evidence-fulfilment";
const TRIGGER_CONSUMER: &str = "clock-sweep";

/// One requirement's run at the held commit, or its refusal.
struct EvidenceRun {
    requirement: ReviewEvidenceRequirement,
    exit_code: Option<i32>,
    timed_out: bool,
    refusal: Option<FulfilmentRefusal>,
    detail: String,
    /// The kind's own log fields: CodeQL's streams and SARIF summary, or a
    /// host test's command line, output and validation environment.
    log: Map<String, Value>,
}

impl EvidenceRun {
    fn new(requirement: &ReviewEvidenceRequirement) -> Self {
        Self {
            requirement: requirement.clone(),
            exit_code: None,
            timed_out: false,
            refusal: None,
            detail: String::new(),
            log: Map::new(),
        }
    }

    fn refused(
        requirement: &ReviewEvidenceRequirement,
        refusal: FulfilmentRefusal,
        detail: String,
    ) -> Self {
        Self {
            refusal: Some(refusal),
            detail,
            ..Self::new(requirement)
        }
    }
}
