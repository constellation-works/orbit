//! Resume a task's preserved candidate instead of re-implementing it
//! [ORB-13985].
//!
//! A task PR run that fails leaves its candidate on an `orbit/<task>-<hash>`
//! branch, and `pr_failure_handoff` records the branch, head, failed step and
//! the task's spec digest on that run. When the task runs again,
//! `candidate_resume` finds that record through the run the task was last
//! linked to and applies the candidate onto the new run's base as
//! uncommitted changes — a squash merge, so the run's own commit step
//! delivers it under the usual gates. Then:
//!
//! - it applies cleanly, the failed step is `commit` or later, and owner
//!   validation passes: `resumed_validated`, and no implementation step runs;
//! - the failed step is the implementation (`implement_bundle` /
//!   `implement_one`) or any step before `commit`: `resumed_repaired`, and
//!   the implementer finishes the applied partial candidate. Validation is
//!   not consulted, so an empty command list cannot accept it;
//! - it conflicts, validation fails, or the before-PR review refused it:
//!   `resumed_repaired`, and the implementer starts from the applied
//!   candidate with that output;
//! - validation could not run for lack of a tool, or fails exactly as it does
//!   on the base [ORB-14258]: `resumed_unjudged`, and no implementation step
//!   runs; the delivery's own validation decides;
//! - there is no usable candidate (none preserved, an operator discarded it,
//!   the spec changed, a bundle, the commit is gone): `fresh`, with the
//!   reason and its `reason_code`.
//!
//! "The spec" is the task's description and acceptance criteria. Context
//! selectors are preparation hints, so a selector edit keeps the candidate;
//! the resumed review reads the current selectors [ORB-14450].
//!
//! [ORB-14450] A run held on named external evidence ends before the failure
//! handoff, so it records no checkpoint. When the task's latest decision is
//! the evidence receipt for that held run, its candidate — the exact commit
//! the task's evidence hold names — is resumed instead: a clean apply is
//! `resumed_held`, and no implementation step or owner validation runs here,
//! so the run goes on to commit, validation and the fresh review that finds
//! the evidence. On the hold's own base the squash reproduces the held tree;
//! on a moved base the review gate counts the evidence only while the patch
//! is unchanged. A conflict hands the implementer the conflict, after which
//! the gate requests the evidence again.
//!
//! Whenever a candidate was found, the outcome, source run and SHA are also
//! written to the task's history.
//!
//! A claimed leaf (`claimed: true`), PR or owner-local, resumes the candidate
//! its owner kept from the task's last claim [ORB-14257] [ORB-14338] instead,
//! handed in as `candidate`: the owner already retired a discarded one, one
//! whose spec changed and one this host cannot fetch, recording why in the
//! task's history, which is the owner's, so none of that is consulted here.
//! A candidate absent from this object store is fetched from `origin` — from
//! the durable ref its leaf carried it to (`durable_ref`), else its branch.
//! The claimed implementer always runs, because the handoff carries its
//! summary: a clean apply is `resumed_repaired` with trigger `continuation`
//! (or `review` when the before-PR review refused it), and the leaf's own
//! validation judges the result.
//!
//! [ORB-14603] When the task's prior run is one another machine executed — a
//! claim's leaf, handed in as `prior_foreign_run` with that machine — its id
//! is never looked up in this machine's run store, where it may name
//! unrelated work. An owner-local run continues the candidate the owner kept
//! from the task's last claim instead, as a claimed leaf would: the owner
//! offers it under the same discard, spec and fetchability checks, a refusal
//! is `fresh` with that typed reason, and every outcome is written to the
//! task's history naming the claim and the machine that committed it. A
//! claim candidate was always committed, so its implementer runs as a
//! claimed leaf's does — unless the task's latest decision is the evidence
//! receipt for the hold that claim's leaf settled into: the owner kept the
//! held candidate from the branch the leaf published it to, and a clean
//! apply is `resumed_held`, as for a local hold. A prior run on this machine
//! that recorded no failure handoff — a claim's leaf this machine executed —
//! falls back to the candidate the owner kept from that run's claim.
//!
//! A repair claim's leaf passes `claim_repair` instead [ORB-14261]: the
//! candidate an owner's landing stopped on a base conflict or stale base.
//! It is squash-merged the same way and the implementer always runs — on a
//! `conflict` to resolve, or on a `landing` repair that applied cleanly onto
//! the moved base. If that candidate cannot be restored, the leaf fails
//! closed instead of implementing fresh and silently dropping its work.

mod apply;
mod lookup;

pub(in crate::executor::automation) use lookup::candidate_resume;

use serde_json::Value;

/// `pr_failure_handoff` decisions that leave a candidate on a branch.
const PRESERVING_DECISIONS: &[&str] = &[
    "blocked_failure_pr",
    "blocked_conflict_pr",
    "blocked_review_gate",
    "awaiting_review_evidence",
    "incomplete_review_timeout",
    "blocked_validation_environment",
    "held_baseline_red",
    "held_provider_failure",
];
/// The settlement step whose failure is the review's verdict on the
/// candidate, not a fault: the repair starts from its findings.
const REVIEW_VERDICT_STEP: &str = "review_gate_settle";
/// Steps of `task_pr_pipeline` and `task_local_pipeline` that run only after
/// `implement_bundle` has finished. A preserved candidate from one of these
/// is a completed implementation and may resume as `resumed_validated`.
///
/// Any other id — `implement_bundle`, the nested `implement_one`, a step
/// before `commit`, or a step this list does not name yet — is unfinished.
/// Unknown ids fail closed so a new pre-commit step cannot skip the
/// implementer. A new step after `commit` belongs here; until it is added,
/// resume hands that candidate to the implementer.
const COMPLETED_IMPLEMENTATION_STEPS: &[&str] = &[
    "commit",
    "prepare_branch",
    "sync_base",
    "validate",
    "review_gate_admit",
    "review",
    "review_gate_settle",
    "review_validate",
    "push",
    "pr_open",
    "promote_tasks",
    "promote_no_diff",
    "landing_review_gate_admit",
    "landing_review",
    "landing_review_gate_settle",
    "landing_review_validate",
    "landing_push",
    "complete_pr",
    "re_review_gate_admit",
    "re_review",
    "re_review_gate_settle",
    "re_review_validate",
    "re_push",
    "complete_reviewed_pr",
    "re_review_gate_admit_2",
    "re_review_2",
    "re_review_gate_settle_2",
    "re_review_validate_2",
    "re_push_2",
    "complete_reviewed_pr_2",
    "complete_no_diff",
    "merge",
    "mark_review",
    "mark_review_one",
    "complete_tasks",
    "complete_one",
];
/// Largest failure output handed to the implementer; the tail is kept, where
/// compilers and test runners report.
const MAX_REPAIR_OUTPUT_BYTES: usize = 32 * 1024;

/// The candidate the task's last run preserved.
struct Candidate {
    run_id: String,
    branch: String,
    head_sha: String,
    /// [ORB-14338] The ref on `origin` a claimed leaf carried the candidate
    /// to, fetched in place of its branch.
    durable_ref: Option<String>,
    failed_step_id: String,
    needs_review_repair: bool,
    /// [ORB-14450] The candidate an evidence hold kept, whose evidence
    /// arrived: resumed without the implementer or owner validation.
    held: bool,
    /// [ORB-14603] The claim whose settlement the owner kept it from, and
    /// the machine that claim executed on, for an owner-local run's resume.
    claim: Option<ClaimSource>,
}

impl Candidate {
    /// The run that produced it, with the machine that executed it when that
    /// was a claim's leaf: its id alone names no run on this machine.
    fn source(&self) -> String {
        match &self.claim {
            Some(claim) => format!("Run '{}' on machine '{}'", self.run_id, claim.machine_id),
            None => format!("Run '{}'", self.run_id),
        }
    }
}

/// The claim a kept candidate came from.
struct ClaimSource {
    claim_id: String,
    machine_id: String,
}

/// [ORB-14603] The task's prior run, executed on another machine.
struct ForeignRun {
    run_id: String,
    machine_id: String,
}

/// What the task's last run left to resume.
enum Preserved {
    /// No candidate, and why.
    None(Fresh),
    /// A candidate that must not be resumed, and why.
    Refused(Candidate, Fresh),
    Usable(Candidate),
}

/// Why the implementer starts from scratch: a stable `code` for consumers and
/// the operator-facing detail.
struct Fresh {
    code: &'static str,
    detail: String,
}

impl Fresh {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

enum Outcome {
    Fresh(Fresh),
    Validated,
    /// [ORB-14450] A held candidate applied cleanly; review decides.
    Held,
    Unjudged(String),
    Repair(Value),
}
