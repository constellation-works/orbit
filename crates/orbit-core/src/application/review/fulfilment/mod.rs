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

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_engine::review_gate::run_host_sandbox_test;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskArtifact, TaskStatus};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    EvidenceHostOs, HostEvidenceReason, HostSandboxCommand, JobRun, JobRunState, JobRunTrigger,
    ReviewEvidenceHold, ReviewEvidenceKind, ReviewEvidenceRequirement, ReviewExternalEvidence,
    ValidationOutcome,
};
use serde_json::{Map, Value, json};

use self::codeql::admitted_codeql_args;
use super::evidence::{evidence_artifact_path, evidence_hold, evidence_ready, hold_is_current};
use crate::OrbitRuntime;
use crate::application::job::pipeline::{
    PipelineSubmission, ROUTINE_DISPATCH_ORBIT_DIR_FIELD, RetryKey,
};
use crate::application::task::{SYSTEM_ACTOR_LABEL, TaskUpdateParams, recovery_checkout_path};

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

/// Why a fulfilment did not attach a result. The hold stays in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FulfilmentRefusal {
    /// The hold is gone, superseded, already satisfied, not this run's, or
    /// names evidence other than `codeql` or a Linux `host_sandbox_test`.
    HoldNotCurrent,
    /// A requirement names a program or command shape outside its kind's
    /// allowlist: anything but the CodeQL script for `codeql`, anything but
    /// `cargo test` or an owner-required command for `host_sandbox_test`.
    CommandNotAllowed,
    /// A `host_sandbox_test` command contains a shell metacharacter, quote,
    /// escape or control character. Never run.
    ShellMetacharacter,
    /// A `host_sandbox_test` argument is outside the `cargo test` grammar.
    /// Never run.
    ArgumentNotAllowed,
    /// An evidence or log locator is invalid or names a reserved review artifact.
    ArtifactNotAllowed,
    /// This process is not the task owner that should fulfil its evidence.
    NotOwner,
    /// This host is not Linux, so no run here can be complete.
    HostNotLinux,
    /// Bubblewrap is unavailable or cannot create its required namespaces.
    SandboxUnavailable,
    /// The scratch filesystem has less free space than the run requires.
    DiskInsufficient,
    /// The held commit could not be fetched, or its tree differs.
    CandidateUnreachable,
    /// A tool the script needs is not on the validation PATH.
    ToolMissing,
    /// The script refused the host platform (exit 3).
    PlatformRefused,
    /// Extraction skipped semantic analysis, or analysis did not complete.
    AnalysisIncomplete,
    /// The command failed for another reason.
    CommandFailed,
    /// The command exceeded its time limit.
    TimedOut,
    /// The run's SARIF could not be read.
    ResultsUnreadable,
    /// The analysis reported results, which a fresh review must judge.
    FindingsReported,
    /// A host sandbox test passed but reported that it skipped or deferred
    /// its confined path, so it is no evidence of that path.
    SelfSkipped,
    /// A `cargo test` host run passed without executing a single test.
    NoTestsRan,
    /// A host sandbox test ran and failed.
    TestFailed,
}

impl FulfilmentRefusal {
    /// Stable name recorded in step output, audit and comments.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HoldNotCurrent => "hold_not_current",
            Self::CommandNotAllowed => "command_not_allowed",
            Self::ShellMetacharacter => "shell_metacharacter",
            Self::ArgumentNotAllowed => "argument_not_allowed",
            Self::ArtifactNotAllowed => "artifact_not_allowed",
            Self::NotOwner => "not_owner",
            Self::HostNotLinux => "host_not_linux",
            Self::SandboxUnavailable => "sandbox_unavailable",
            Self::DiskInsufficient => "disk_insufficient",
            Self::CandidateUnreachable => "candidate_unreachable",
            Self::ToolMissing => "tool_missing",
            Self::PlatformRefused => "platform_refused",
            Self::AnalysisIncomplete => "analysis_incomplete",
            Self::CommandFailed => "command_failed",
            Self::TimedOut => "timed_out",
            Self::ResultsUnreadable => "results_unreadable",
            Self::FindingsReported => "findings_reported",
            Self::SelfSkipped => "self_skipped",
            Self::NoTestsRan => "no_tests_ran",
            Self::TestFailed => "test_failed",
        }
    }

    /// The refusal a host sandbox test's typed reason is here. An OS
    /// mismatch cannot reach a run: only Linux requirements are fulfilled.
    fn of_host(reason: HostEvidenceReason) -> Self {
        match reason {
            HostEvidenceReason::ShellMetacharacter => Self::ShellMetacharacter,
            HostEvidenceReason::CommandNotAllowed => Self::CommandNotAllowed,
            HostEvidenceReason::ArgumentNotAllowed => Self::ArgumentNotAllowed,
            HostEvidenceReason::OsMismatch => Self::HoldNotCurrent,
            HostEvidenceReason::CandidateChanged => Self::CandidateUnreachable,
            HostEvidenceReason::SandboxUnavailable => Self::SandboxUnavailable,
            HostEvidenceReason::SelfSkipped => Self::SelfSkipped,
            HostEvidenceReason::NoTestsRan => Self::NoTestsRan,
            HostEvidenceReason::ToolMissing => Self::ToolMissing,
            HostEvidenceReason::TimedOut => Self::TimedOut,
            HostEvidenceReason::RunFailed => Self::CommandFailed,
            HostEvidenceReason::TestFailed => Self::TestFailed,
        }
    }

    /// Whether a later tick may try the same hold again.
    fn retryable(self) -> bool {
        matches!(self, Self::DiskInsufficient | Self::CandidateUnreachable)
    }
}

/// What one fulfilment tick did.
#[derive(Debug, Default)]
pub struct EvidenceFulfilmentTick {
    /// Why the tick did nothing, when it stood down.
    pub skipped: Option<String>,
    /// `(task id, run id)` for each fulfilment it started.
    pub dispatched: Vec<(String, String)>,
}

/// A requirement's command as fulfilment admitted it.
enum AdmittedCommand {
    /// The CodeQL script's arguments.
    Codeql(Vec<String>),
    HostSandboxTest(HostSandboxCommand),
}

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

impl OrbitRuntime {
    /// Why this runtime fulfils no evidence hold, if it does not: a claimed
    /// worker, a replica checkout, or a host other than Linux.
    pub fn review_evidence_fulfilment_disabled_reason(&self) -> Option<String> {
        self.fulfilment_disabled_refusal().map(|(_, reason)| reason)
    }

    fn fulfilment_disabled_refusal(&self) -> Option<(FulfilmentRefusal, String)> {
        self.fulfilment_owner_refusal()
            .or_else(|| self.fulfilment_host_refusal())
    }

    /// Why this process is not the owner that fulfils its tasks' evidence.
    fn fulfilment_owner_refusal(&self) -> Option<(FulfilmentRefusal, String)> {
        if self.worker_invocation().is_some() {
            return Some((
                FulfilmentRefusal::NotOwner,
                "a claimed worker never fulfils evidence; its owner does".to_string(),
            ));
        }
        if let Some(owner) = self.coordination_write_owner() {
            return Some((
                FulfilmentRefusal::NotOwner,
                format!(
                    "this replica checkout does not own its task records; machine '{owner}' \
                     fulfils their evidence"
                ),
            ));
        }
        None
    }

    /// Why this host cannot produce Linux evidence: another platform, or no
    /// Bubblewrap namespaces, which both CodeQL's confinement and every
    /// sandbox-gated test need.
    fn fulfilment_host_refusal(&self) -> Option<(FulfilmentRefusal, String)> {
        if std::env::consts::OS != "linux" {
            return Some((
                FulfilmentRefusal::HostNotLinux,
                format!(
                    "host platform {} cannot run Linux CodeQL or Linux sandbox tests",
                    std::env::consts::OS
                ),
            ));
        }
        let bwrap = orbit_exec::probe_bwrap();
        if !bwrap.available {
            return Some((
                FulfilmentRefusal::SandboxUnavailable,
                format!("Bubblewrap is unavailable: {}", bwrap.detail),
            ));
        }
        None
    }

    /// One fulfilment tick: dispatch a run for each held task whose evidence
    /// this host can produce, at most [`MAX_ACTIVE_EVIDENCE_FULFILMENTS`]
    /// live at once.
    pub fn run_review_evidence_fulfilment_tick(
        &self,
        now: DateTime<Utc>,
    ) -> Result<EvidenceFulfilmentTick, OrbitError> {
        let orbit_dir = self.shared_root();
        let mut tick = EvidenceFulfilmentTick::default();
        if let Some(reason) = self.review_evidence_fulfilment_disabled_reason() {
            tick.skipped = Some(reason);
            return Ok(tick);
        }
        let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            job_id: Some(REVIEW_EVIDENCE_FULFILMENT_JOB.to_string()),
            limit: Some(RUN_SCAN_LIMIT),
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        let active = runs
            .iter()
            .filter(|run| !run.state.is_terminal() && run.state != JobRunState::Skipped)
            .count();
        let mut capacity = MAX_ACTIVE_EVIDENCE_FULFILMENTS.saturating_sub(active);
        if capacity == 0 {
            return Ok(tick);
        }
        // Defer rather than spend a hold's attempts on a run that would only
        // refuse for disk; the run re-checks against its own threshold.
        let min_free_mib = self
            .resolved_job_spec(REVIEW_EVIDENCE_FULFILMENT_JOB)?
            .default_input
            .as_ref()
            .and_then(min_free_mib_of)
            .unwrap_or(DEFAULT_FULFILMENT_MIN_FREE_MIB);
        let free = free_mib(&self.paths().state_dir)?;
        if free < min_free_mib {
            tick.skipped = Some(format!(
                "{free} MiB free under the state directory; a fulfilment needs {min_free_mib}"
            ));
            return Ok(tick);
        }
        for task in
            self.list_tasks_filtered(Some(TaskStatus::InProgress), None, None, None, None, None)?
        {
            if capacity == 0 {
                break;
            }
            let hold = match fulfilable_hold(self, &task) {
                Ok(Some(hold)) => hold,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        target: "orbit.core.review",
                        task_id = %task.id,
                        "cannot read the review evidence hold: {error}"
                    );
                    continue;
                }
            };
            let key = hold_key(&hold);
            let Some(attempt) = self.next_fulfilment_attempt(&runs, &key)? else {
                continue;
            };
            let input = json!({
                "task_id": task.id,
                HOLD_KEY_FIELD: key,
                ATTEMPT_KEY_FIELD: format!("{key}#{attempt}"),
                ROUTINE_DISPATCH_ORBIT_DIR_FIELD: orbit_dir.to_string_lossy(),
                "observed_at": now.to_rfc3339(),
            });
            let submission = PipelineSubmission {
                retry_key: Some(RetryKey {
                    field: ATTEMPT_KEY_FIELD,
                    scan_limit: RUN_SCAN_LIMIT,
                }),
                trigger: JobRunTrigger::state_routine(TRIGGER_NAME, TRIGGER_CONSUMER),
                ..PipelineSubmission::catalog(
                    REVIEW_EVIDENCE_FULFILMENT_JOB,
                    input,
                    Some(SYSTEM_ACTOR_LABEL),
                )
            };
            match self.submit_keyed_pipeline_run(submission) {
                Ok((result, _)) => {
                    tick.dispatched.push((task.id.to_string(), result.run_id));
                    capacity -= 1;
                }
                Err(error) => tracing::warn!(
                    target: "orbit.core.review",
                    task_id = %task.id,
                    "failed to dispatch review evidence fulfilment: {error}"
                ),
            }
        }
        Ok(tick)
    }

    /// The attempt number the next run for `key` takes, or `None` when the
    /// hold already has a live run, a run that ended on a final outcome, or
    /// every attempt. A run that ended without recording an outcome (its
    /// worker died or was interrupted) counts as an attempt, not a decision.
    fn next_fulfilment_attempt(
        &self,
        runs: &[JobRun],
        key: &str,
    ) -> Result<Option<usize>, OrbitError> {
        let mut attempts = 0;
        for run in runs
            .iter()
            .filter(|run| run_input_field(run, HOLD_KEY_FIELD) == Some(key))
        {
            attempts += 1;
            if !run.state.is_terminal() {
                return Ok(None);
            }
            let retryable = self
                .read_run_state(&run.run_id)?
                .and_then(|state| state.pipeline.get(FULFIL_STEP).cloned())
                .is_none_or(|output| {
                    output.get("retryable").and_then(Value::as_bool) == Some(true)
                });
            if !retryable {
                return Ok(None);
            }
        }
        Ok((attempts < MAX_FULFILMENT_ATTEMPTS).then_some(attempts + 1))
    }

    /// The fulfilment step: re-check the hold, admit every command, gate on
    /// host and disk, run each named command at the held commit, and attach
    /// the logs — plus the results, when every run passed. Always audited.
    pub(crate) fn fulfil_review_evidence(
        &self,
        input: &Value,
        run_id: &str,
    ) -> Result<Value, OrbitError> {
        let task_id = input
            .get("task_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| OrbitError::InvalidInput("`task_id` is required".into()))?;
        let key = input
            .get(HOLD_KEY_FIELD)
            .and_then(Value::as_str)
            .unwrap_or_default();
        let min_free_mib = min_free_mib_of(input).unwrap_or(DEFAULT_FULFILMENT_MIN_FREE_MIB);
        let task = self.get_task(task_id)?;
        let hold = fulfilable_hold(self, &task)?.filter(|hold| hold_key(hold) == key);
        let outcome = match &hold {
            None => Err((FulfilmentRefusal::HoldNotCurrent, String::new())),
            Some(hold) => self.fulfil_hold(&task, hold, run_id, min_free_mib),
        };
        let free_mib = free_mib(&self.paths().state_dir).ok();
        let (refusal, detail, runs) = match outcome {
            Ok(runs) => (
                runs.iter().find_map(|run| run.refusal),
                runs.iter()
                    .find(|run| run.refusal.is_some())
                    .map(|run| run.detail.clone())
                    .unwrap_or_default(),
                runs,
            ),
            Err((refusal, detail)) => (Some(refusal), detail, Vec::new()),
        };
        let mut requeued = false;
        if let Some(hold) = &hold
            && refusal != Some(FulfilmentRefusal::HoldNotCurrent)
        {
            self.attach_fulfilment(&task, hold, run_id, &runs, refusal, &detail)?;
            requeued = self.get_task(task_id)?.status == TaskStatus::Backlog;
        }
        let output = json!({
            "task_id": task_id,
            HOLD_KEY_FIELD: key,
            "fulfilled": refusal.is_none(),
            "reason": refusal.map(FulfilmentRefusal::as_str),
            "detail": detail,
            "retryable": refusal.is_some_and(FulfilmentRefusal::retryable),
            "requeued": requeued,
        });
        self.record_pipeline_audit(
            EVIDENCE_FULFILMENT_AUDIT,
            Some(run_id),
            Some(SYSTEM_ACTOR_LABEL),
            match refusal {
                None => AuditEventStatus::Success,
                Some(FulfilmentRefusal::HoldNotCurrent) => AuditEventStatus::Denied,
                Some(_) => AuditEventStatus::Failure,
            },
            json!({
                "run_id": run_id,
                "task_id": task_id,
                "attempt_id": hold.as_ref().map(|hold| hold.attempt_id.clone()),
                "candidate": hold.as_ref().map(|hold| hold.candidate.commit.clone()),
                "commands": runs.iter().map(|run| &run.requirement.command).collect::<Vec<_>>(),
                "exit_codes": runs.iter().map(|run| run.exit_code).collect::<Vec<_>>(),
                "free_mib": free_mib,
                "min_free_mib": min_free_mib,
                "outcome": if refusal.is_none() { "fulfilled" } else { "refused" },
                "reason": refusal.map(FulfilmentRefusal::as_str),
                "requeued": requeued,
                "recorded_at": Utc::now().to_rfc3339(),
            }),
            refusal.map(|refusal| format!("{}: {detail}", refusal.as_str())),
        )?;
        Ok(output)
    }

    /// Admit, gate, fetch, check out and run. `Err` is a refusal before any
    /// command ran; `Ok` holds each command's run, refused or not. A command
    /// outside its allowlist refuses the whole hold before any host gate, so
    /// nothing runs and only the refused commands' logs are attached.
    fn fulfil_hold(
        &self,
        task: &Task,
        hold: &ReviewEvidenceHold,
        run_id: &str,
        min_free_mib: u64,
    ) -> Result<Vec<EvidenceRun>, (FulfilmentRefusal, String)> {
        // Persisted holds can predate review admission's path guards. Refuse
        // unsafe locators before running any candidate-controlled script.
        for requirement in &hold.requirements {
            fulfilment_artifact_paths(&requirement.artifact)
                .map_err(|error| (FulfilmentRefusal::ArtifactNotAllowed, error.to_string()))?;
        }
        if let Some((refusal, reason)) = self.fulfilment_owner_refusal() {
            return Err((refusal, reason));
        }
        let owner_required = self.workflow_required_validation_commands();
        let admitted = hold
            .requirements
            .iter()
            .map(|requirement| admit_requirement(requirement, owner_required))
            .collect::<Vec<_>>();
        if admitted.iter().any(Result::is_err) {
            return Ok(hold
                .requirements
                .iter()
                .zip(admitted)
                .filter_map(|(requirement, admitted)| {
                    let (refusal, detail) = admitted.err()?;
                    Some(EvidenceRun::refused(requirement, refusal, detail))
                })
                .collect());
        }
        if let Some((refusal, reason)) = self.fulfilment_host_refusal() {
            return Err((refusal, reason));
        }
        let free = free_mib(&self.paths().state_dir)
            .map_err(|error| (FulfilmentRefusal::DiskInsufficient, error.to_string()))?;
        if free < min_free_mib {
            return Err((
                FulfilmentRefusal::DiskInsufficient,
                format!("{free} MiB free under the state directory; the run needs {min_free_mib}"),
            ));
        }
        let repo_root = &self.paths().repo_root;
        let commit = &hold.candidate.commit;
        let reachable = orbit_engine::review_gate::fetch_landed_commit(repo_root, commit)
            .and_then(|()| orbit_engine::review_gate::revision(repo_root, commit));
        match reachable {
            Ok(revision) if revision == hold.candidate => {}
            Ok(revision) => {
                return Err((
                    FulfilmentRefusal::CandidateUnreachable,
                    format!(
                        "commit {commit} has tree {}, not the held {}",
                        revision.tree, hold.candidate.tree
                    ),
                ));
            }
            Err(error) => return Err((FulfilmentRefusal::CandidateUnreachable, error.to_string())),
        }
        let admitted = admitted.into_iter().flatten().collect::<Vec<_>>();
        let checkout_id = format!("{run_id}-evidence");
        let checkout = if admitted
            .iter()
            .any(|admitted| matches!(admitted, AdmittedCommand::Codeql(_)))
        {
            Some(
                self.create_evidence_checkout(&checkout_id, commit)
                    .map_err(|error| {
                        (FulfilmentRefusal::CandidateUnreachable, error.to_string())
                    })?,
            )
        } else {
            None
        };
        let target_id = format!("{run_id}-evidence-target");
        let runs = hold
            .requirements
            .iter()
            .zip(admitted)
            .map(|(requirement, admitted)| match (admitted, &checkout) {
                (AdmittedCommand::Codeql(args), Some(checkout)) => {
                    self.run_codeql(checkout, requirement, args)
                }
                (AdmittedCommand::Codeql(_), None) => EvidenceRun::refused(
                    requirement,
                    FulfilmentRefusal::CandidateUnreachable,
                    "no evidence checkout was prepared".to_string(),
                ),
                (AdmittedCommand::HostSandboxTest(command), _) => {
                    self.run_host_test(hold, run_id, &target_id, requirement, &command)
                }
            })
            .collect::<Vec<_>>();
        // The checkout and test target are this run's own scratch, database
        // and build included; remove them whatever happened so a held host
        // never accumulates them.
        for scratch in [&checkout_id, &target_id] {
            if let Err(error) = self.remove_evidence_checkout(scratch) {
                tracing::warn!(
                    target: "orbit.core.review",
                    task_id = %task.id,
                    "failed to remove the evidence scratch {scratch}: {error}"
                );
            }
        }
        Ok(runs)
    }

    /// Remove a fulfilment run's checkout or test target, if it exists. It is
    /// the run's own directory, registered nowhere else.
    fn remove_evidence_checkout(&self, checkout_id: &str) -> Result<(), OrbitError> {
        let path = recovery_checkout_path(&self.paths().state_dir, checkout_id)?;
        match std::fs::remove_dir_all(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(OrbitError::Execution(format!(
                "remove evidence checkout {}: {error}",
                path.display()
            ))),
        }
    }

    /// Run one admitted host sandbox test at the held commit, outside any
    /// sandbox, and judge it with the contract a claimed leaf uses.
    fn run_host_test(
        &self,
        hold: &ReviewEvidenceHold,
        run_id: &str,
        target_id: &str,
        requirement: &ReviewEvidenceRequirement,
        command: &HostSandboxCommand,
    ) -> EvidenceRun {
        let target = match recovery_checkout_path(&self.paths().state_dir, target_id) {
            Ok(target) => target,
            Err(error) => {
                return EvidenceRun::refused(
                    requirement,
                    FulfilmentRefusal::CommandFailed,
                    error.to_string(),
                );
            }
        };
        let ran = run_host_sandbox_test(
            self,
            &self.paths().repo_root,
            &hold.candidate,
            command,
            run_id,
            &target,
        );
        let ran = match ran {
            Ok(ran) => ran,
            Err(error) => {
                return EvidenceRun::refused(
                    requirement,
                    FulfilmentRefusal::CommandFailed,
                    error.to_string(),
                );
            }
        };
        let mut run = EvidenceRun::new(requirement);
        run.exit_code = ran.exit_code;
        run.timed_out = ran.timed_out;
        run.log.extend([
            ("kind".to_string(), json!(requirement.kind)),
            ("os".to_string(), json!(requirement.os)),
            ("host_command".to_string(), json!(ran.host_command)),
            (
                "tests_passed".to_string(),
                json!(ran.judgement.as_ref().ok()),
            ),
            ("output".to_string(), json!(ran.output)),
            ("validation_env".to_string(), ran.environment),
        ]);
        if let Err(refusal) = ran.judgement {
            run.refusal = Some(FulfilmentRefusal::of_host(refusal.reason));
            run.detail = refusal.detail;
        }
        run
    }

    /// Attach each run's log, and every result when all of them passed, in
    /// one write: receipt then requeues the task for a fresh review. A refused
    /// fulfilment attaches only logs, so nothing is accepted.
    fn attach_fulfilment(
        &self,
        task: &Task,
        hold: &ReviewEvidenceHold,
        run_id: &str,
        runs: &[EvidenceRun],
        refusal: Option<FulfilmentRefusal>,
        detail: &str,
    ) -> Result<(), OrbitError> {
        let mut artifacts = Vec::new();
        for run in runs {
            // Check both final store keys at the system-authority write boundary.
            // Build the whole batch first, so a refusal cannot partially write it.
            let (artifact, log_artifact) = fulfilment_artifact_paths(&run.requirement.artifact)?;
            let mut log = json!({
                "schema_version": 1,
                "run_id": run_id,
                "task_id": task.id,
                "attempt_id": hold.attempt_id,
                "tested_head": hold.candidate.commit,
                "tested_tree": hold.candidate.tree,
                "command": run.requirement.command,
                "exit_code": run.exit_code,
                "timed_out": run.timed_out,
                "outcome": if run.refusal.is_none() { "passed" } else { "failed" },
                "reason": run.refusal.map(FulfilmentRefusal::as_str),
                "detail": run.detail,
                "host_os": std::env::consts::OS,
                "recorded_at": Utc::now().to_rfc3339(),
            });
            if let Some(log) = log.as_object_mut() {
                log.extend(run.log.clone());
            }
            artifacts.push(json_artifact(&log_artifact, &log)?);
            if refusal.is_none() {
                let evidence = ReviewExternalEvidence {
                    schema_version: 1,
                    attempt_id: hold.attempt_id.clone(),
                    candidate: hold.candidate.clone(),
                    kind: run.requirement.kind,
                    name: run.requirement.name.clone(),
                    command: run.requirement.command.clone(),
                    outcome: ValidationOutcome::Passed,
                    log_artifact,
                    os: EvidenceHostOs::current(),
                };
                artifacts.push(json_artifact(&artifact, &evidence)?);
            }
        }
        let comment = match refusal {
            None => format!(
                "Owner evidence fulfilment run={run_id} ran every named check at held candidate \
                 `{}` on this Linux host and attached the results and logs. A fresh review \
                 verifies them; nothing was approved.",
                hold.candidate.commit
            ),
            Some(refusal) => format!(
                "Owner evidence fulfilment run={run_id} did not attach a result for held \
                 candidate `{}`: `{}`{}. The evidence hold stays in place{}.",
                hold.candidate.commit,
                refusal.as_str(),
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({detail})")
                },
                if refusal.retryable() {
                    "; a later tick retries it"
                } else {
                    ""
                },
            ),
        };
        self.update_task_as_system(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: artifacts,
                comment: Some(comment),
                ..TaskUpdateParams::default()
            },
            Some(run_id.to_string()),
        )?;
        Ok(())
    }
}

/// `fulfil_review_evidence`: the only step of
/// [`REVIEW_EVIDENCE_FULFILMENT_JOB`]. A refusal is a successful step whose
/// output names the reason; only an Orbit failure fails the step.
pub(crate) fn fulfil_review_evidence(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    let run_id =
        run_id.ok_or_else(|| failed("a fulfilment step must run inside a pipeline run".into()))?;
    runtime
        .fulfil_review_evidence(input, run_id)
        .map_err(|error| failed(error.to_string()))
}

/// The hold on `task` this owner can fulfil: current, every requirement a
/// `codeql` run or a Linux `host_sandbox_test`, and its evidence not yet
/// arrived.
fn fulfilable_hold(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<Option<ReviewEvidenceHold>, OrbitError> {
    if task.status != TaskStatus::InProgress {
        return Ok(None);
    }
    let Some(hold) = evidence_hold(runtime, &task.id)? else {
        return Ok(None);
    };
    if hold.requirements.is_empty()
        || !hold.requirements.iter().all(owner_fulfils)
        || !hold_is_current(runtime, task, &hold)?
        || evidence_ready(runtime, &task.id, &hold)?
    {
        return Ok(None);
    }
    Ok(Some(hold))
}

/// Whether a Linux owner produces this requirement's evidence.
fn owner_fulfils(requirement: &ReviewEvidenceRequirement) -> bool {
    match requirement.kind {
        ReviewEvidenceKind::CodeQl => true,
        ReviewEvidenceKind::HostSandboxTest => requirement.os == Some(EvidenceHostOs::Linux),
        ReviewEvidenceKind::HostedCi | ReviewEvidenceKind::NativeOs => false,
    }
}

/// Admit `requirement`'s command or refuse it with a typed reason. Nothing
/// outside its kind's closed allowlist ever runs.
fn admit_requirement(
    requirement: &ReviewEvidenceRequirement,
    owner_required: &[String],
) -> Result<AdmittedCommand, (FulfilmentRefusal, String)> {
    match requirement.kind {
        ReviewEvidenceKind::CodeQl => admitted_codeql_args(&requirement.command)
            .map(AdmittedCommand::Codeql)
            .map_err(|detail| (FulfilmentRefusal::CommandNotAllowed, detail)),
        ReviewEvidenceKind::HostSandboxTest if requirement.os == Some(EvidenceHostOs::Linux) => {
            HostSandboxCommand::admit(&requirement.command, owner_required)
                .map(AdmittedCommand::HostSandboxTest)
                .map_err(|refusal| (FulfilmentRefusal::of_host(refusal.reason), refusal.detail))
        }
        _ => Err((
            FulfilmentRefusal::HoldNotCurrent,
            format!("a Linux owner does not fulfil `{}`", requirement.name),
        )),
    }
}

/// One hold's identity: its attempt and exact candidate.
fn hold_key(hold: &ReviewEvidenceHold) -> String {
    format!("{}:{}", hold.attempt_id, hold.candidate.commit)
}

fn run_input_field<'a>(run: &'a JobRun, field: &str) -> Option<&'a str> {
    run.input.as_ref()?.get(field)?.as_str()
}

/// `evidence/x.json` logs to `evidence/x.log.json`.
fn log_artifact_path(artifact: &str) -> String {
    match artifact.strip_suffix(".json") {
        Some(stem) => format!("{stem}.log.json"),
        None => format!("{artifact}.log.json"),
    }
}

pub(super) fn fulfilment_artifact_paths(artifact: &str) -> Result<(String, String), OrbitError> {
    let refused = || {
        OrbitError::InvalidInput(
            "external evidence and log artifacts must have valid, non-reserved review paths"
                .to_string(),
        )
    };
    let artifact = evidence_artifact_path(artifact).ok_or_else(refused)?;
    let log = evidence_artifact_path(&log_artifact_path(&artifact)).ok_or_else(refused)?;
    Ok((artifact, log))
}

fn json_artifact(path: &str, value: &impl serde::Serialize) -> Result<TaskArtifact, OrbitError> {
    Ok(TaskArtifact {
        path: path.to_string(),
        content: serde_json::to_vec_pretty(value)
            .map_err(|error| OrbitError::Execution(format!("serialize {path}: {error}")))?,
        media_type: "application/json".to_string(),
        created_by: None,
    })
}

/// A `min_free_mib` input field, rendered as a number or numeric string.
fn min_free_mib_of(input: &Value) -> Option<u64> {
    match input.get("min_free_mib")? {
        Value::String(text) => text.trim().parse().ok(),
        value => value.as_u64(),
    }
}

fn free_mib(path: &Path) -> Result<u64, OrbitError> {
    fs2::available_space(path)
        .map(|bytes| bytes / (1024 * 1024))
        .map_err(|error| OrbitError::Io(format!("free space under {}: {error}", path.display())))
}
