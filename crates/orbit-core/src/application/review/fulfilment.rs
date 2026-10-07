//! Owner-side fulfilment of a review evidence hold whose every requirement is
//! a Linux CodeQL run.
//!
//! A non-Linux reviewer cannot complete `scripts/codeql-rust-local.sh` (it
//! exits 3), so it holds the review for a `codeql` result instead. On a Linux
//! owner, each clock tick finds those holds and dispatches
//! `review_evidence_fulfilment_pipeline`, whose one step runs the named
//! command at the held commit in a standalone scratch checkout and attaches the
//! result and its log. Receipt of every matching result then queues the task
//! for a fresh review through [`super::evidence::resume_evidence_hold`]; a
//! fulfilment never approves a candidate.
//!
//! A hold is fulfilled only when:
//!
//! - it is still the in-progress task's latest decision
//!   ([`super::evidence::hold_is_current`]) and its evidence has not arrived;
//! - every requirement is kind `codeql` and names the local CodeQL script
//!   with nothing but its own options and one query selector, so a hold can
//!   never make the owner run another command;
//! - the host is Linux with working Bubblewrap namespaces, and the filesystem
//!   holding the scratch checkout has at least the run's `min_free_mib` free;
//! - the held commit, fetched from `origin` when the owner lacks it, has the
//!   held tree.
//!
//! A run that exits nonzero, reports incomplete extraction, times out, leaves
//! no complete SARIF, or reports any result attaches its log but no result,
//! so the hold stays in place with a typed reason. Every attempt is audited
//! under [`EVIDENCE_FULFILMENT_AUDIT`] and commented on the task. At most
//! [`MAX_ACTIVE_EVIDENCE_FULFILMENTS`] run at once; a hold gets one run
//! unless a run refused it for a reason that can clear (disk, fetch) or ended
//! without an outcome, up to [`MAX_FULFILMENT_ATTEMPTS`].

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, Stdio};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_common::text::ceil_char_boundary;
use orbit_engine::DispatchError;
#[cfg(target_os = "linux")]
use orbit_exec::{EnvironmentMode, ExecRequest, Sandbox, StdinMode, run_process};
use orbit_store::contracts::JobRunQuery;
#[cfg(target_os = "linux")]
use orbit_types::policy::ResolvedFsProfile;
use orbit_types::task::{Task, TaskArtifact, TaskStatus};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    JobRun, JobRunState, JobRunTrigger, ReviewEvidenceHold, ReviewEvidenceKind,
    ReviewEvidenceRequirement, ReviewExternalEvidence, ValidationOutcome,
};
use serde_json::{Value, json};

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
/// Fulfilment runs live at once; a CodeQL run is heavy on memory and disk.
pub(crate) const MAX_ACTIVE_EVIDENCE_FULFILMENTS: usize = 1;
/// Runs one hold may have: the first, plus reruns after a refusal that can
/// clear without a decision (not enough disk, an unreachable candidate).
pub(crate) const MAX_FULFILMENT_ATTEMPTS: usize = 3;
/// Free space the scratch checkout's filesystem needs before a run starts,
/// when neither the job's `default_input` nor the run input names one: a
/// database, toolchain and Cargo build.
pub(crate) const DEFAULT_FULFILMENT_MIN_FREE_MIB: u64 = 30 * 1024;
/// The only command a fulfilment runs, relative to the held checkout.
const CODEQL_SCRIPT: &str = "scripts/codeql-rust-local.sh";
/// Ceiling for one CodeQL run: toolchain preparation, extraction, analysis.
const CODEQL_TIMEOUT_MS: u64 = 3 * 60 * 60 * 1000;
/// Captured output kept per stream in the log artifact.
const MAX_CAPTURED_STREAM_BYTES: usize = 128 * 1024;
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
/// The script's own markers: where it keeps this run, and that analysis ran.
const RUN_DIRECTORY_MARKER: &str = "codeql-rust-local: run directory: ";
const ANALYSIS_COMPLETED_MARKER: &str = "codeql-rust-local: analysis completed";

/// Why a fulfilment did not attach a result. The hold stays in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FulfilmentRefusal {
    /// The hold is gone, superseded, already satisfied, not this run's, or
    /// names evidence other than `codeql`.
    HoldNotCurrent,
    /// A requirement names a command other than the CodeQL script's.
    CommandNotAllowed,
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
}

impl FulfilmentRefusal {
    /// Stable name recorded in step output, audit and comments.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::HoldNotCurrent => "hold_not_current",
            Self::CommandNotAllowed => "command_not_allowed",
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

/// One CodeQL requirement's run at the held commit.
struct CodeqlRun {
    requirement: ReviewEvidenceRequirement,
    exit_code: Option<i32>,
    timed_out: bool,
    stdout: String,
    stderr: String,
    sarif: Option<Value>,
    refusal: Option<FulfilmentRefusal>,
    detail: String,
}

/// Bubblewrap confines candidate-controlled CodeQL scripts to the disposable
/// evidence checkout. The checkout is the only writable task-controlled path;
/// Cargo's ambient download caches are remounted read-only because this script
/// uses a run-local `CARGO_HOME`.
#[cfg(target_os = "linux")]
struct EvidenceCodeqlSandbox {
    checkout: PathBuf,
    profile: ResolvedFsProfile,
}

#[cfg(target_os = "linux")]
impl EvidenceCodeqlSandbox {
    fn new(checkout: &Path) -> Self {
        let checkout = checkout.to_path_buf();
        let mut modify = vec![checkout.display().to_string()];
        for cargo_home in cargo_home_candidates() {
            for relative in ["registry", "git"] {
                let path = cargo_home.join(relative);
                if path.exists() {
                    modify.push(format!("!{}/**", path.display()));
                }
            }
            for relative in [".package-cache", ".package-cache-mutate"] {
                let path = cargo_home.join(relative);
                if path.exists() {
                    modify.push(format!("!{}", path.display()));
                }
            }
        }
        Self {
            profile: ResolvedFsProfile {
                name: "review-evidence-fulfilment".to_string(),
                read: Vec::new(),
                modify,
            },
            checkout,
        }
    }
}

#[cfg(target_os = "linux")]
fn cargo_home_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = std::env::var_os("CARGO_HOME").filter(|home| !home.is_empty()) {
        candidates.push(PathBuf::from(home));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        candidates.push(PathBuf::from(home).join(".cargo"));
    }
    let current_dir = std::env::current_dir().ok();
    let mut canonical = std::collections::BTreeSet::new();
    candidates
        .into_iter()
        .filter_map(|path| {
            let absolute = if path.is_absolute() {
                path
            } else {
                current_dir.as_ref()?.join(path)
            };
            let path = absolute.canonicalize().ok()?;
            path.is_dir().then_some(path)
        })
        .filter(|path| canonical.insert(path.clone()))
        .collect()
}

#[cfg(target_os = "linux")]
impl Sandbox for EvidenceCodeqlSandbox {
    fn validate(&self, request: &ExecRequest) -> Result<(), OrbitError> {
        if request.current_dir.as_deref() != Some(self.checkout.to_string_lossy().as_ref()) {
            return Err(OrbitError::PolicyDenied(
                "CodeQL must run from its evidence checkout".to_string(),
            ));
        }
        if request.program != self.checkout.join(CODEQL_SCRIPT).to_string_lossy().as_ref() {
            return Err(OrbitError::PolicyDenied(
                "CodeQL program must be the admitted checkout script".to_string(),
            ));
        }
        Ok(())
    }

    fn spawn(&self, request: &ExecRequest) -> Result<Child, OrbitError> {
        let environment = match &request.environment_mode {
            EnvironmentMode::ClearAndSet(environment) => environment.clone(),
            EnvironmentMode::Inherit => {
                return Err(OrbitError::PolicyDenied(
                    "CodeQL sandbox requires an explicit environment".to_string(),
                ));
            }
        };
        let stdin = match &request.stdin_mode {
            StdinMode::Inherit => Stdio::inherit(),
            StdinMode::Null => Stdio::null(),
            StdinMode::Bytes(_) => Stdio::piped(),
        };
        let mut plan = orbit_exec::compile_linux_bwrap_argv(
            &self.profile,
            &request.program,
            &request.args,
            Some(&self.checkout),
            true,
        )?;
        if !plan.dropped_grants.is_empty() {
            return Err(OrbitError::PolicyDenied(format!(
                "CodeQL sandbox could not enforce writable checkout grants: {:?}",
                plan.dropped_grants
            )));
        }
        if let Some(guard) = plan.take_post_run_guard() {
            // This profile has only exact subtree rules whose writable roots
            // exist, so no post-run check is expected. Refuse instead of
            // silently dropping a future policy boundary.
            return Err(OrbitError::PolicyDenied(format!(
                "CodeQL sandbox unexpectedly needs a post-run guard: {guard:?}"
            )));
        }
        let child = orbit_exec::spawn_under_linux_bwrap(orbit_exec::LinuxBwrapSpawnRequest {
            plan: &plan,
            env: &environment,
            cwd: Some(&self.checkout),
            stdin,
            stdout: Stdio::piped(),
            stderr: Stdio::piped(),
        })?;
        Ok(child)
    }
}

impl OrbitRuntime {
    /// Why this runtime fulfils no evidence hold, if it does not: a claimed
    /// worker, a replica checkout, or a host other than Linux.
    pub fn review_evidence_fulfilment_disabled_reason(&self) -> Option<String> {
        self.fulfilment_disabled_refusal().map(|(_, reason)| reason)
    }

    fn fulfilment_disabled_refusal(&self) -> Option<(FulfilmentRefusal, String)> {
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
        if std::env::consts::OS != "linux" {
            return Some((
                FulfilmentRefusal::HostNotLinux,
                format!(
                    "host platform {} cannot run a complete Rust CodeQL extraction",
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

    /// The fulfilment step: re-check the hold, gate on host and disk, run
    /// each named CodeQL command at the held commit, and attach the logs —
    /// plus the results, when every run passed. Always audited.
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

    /// Gate, fetch, check out and run. `Err` is a refusal before any command
    /// ran; `Ok` holds each command's run, refused or not.
    fn fulfil_hold(
        &self,
        task: &Task,
        hold: &ReviewEvidenceHold,
        run_id: &str,
        min_free_mib: u64,
    ) -> Result<Vec<CodeqlRun>, (FulfilmentRefusal, String)> {
        // Persisted holds can predate review admission's path guards. Refuse
        // unsafe locators before running any candidate-controlled script.
        for requirement in &hold.requirements {
            fulfilment_artifact_paths(&requirement.artifact)
                .map_err(|error| (FulfilmentRefusal::ArtifactNotAllowed, error.to_string()))?;
        }
        if let Some((refusal, reason)) = self.fulfilment_disabled_refusal() {
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
        let checkout_id = format!("{run_id}-evidence");
        let checkout = self
            .create_evidence_checkout(&checkout_id, commit)
            .map_err(|error| (FulfilmentRefusal::CandidateUnreachable, error.to_string()))?;
        let runs = hold
            .requirements
            .iter()
            .map(|requirement| self.run_codeql(&checkout, requirement))
            .collect::<Vec<_>>();
        // The checkout is this run's own scratch, database included; remove
        // it whatever happened so a held host never accumulates them.
        if let Err(error) = self.remove_evidence_checkout(&checkout_id) {
            tracing::warn!(
                target: "orbit.core.review",
                task_id = %task.id,
                "failed to remove the evidence checkout {}: {error}",
                checkout.display()
            );
        }
        Ok(runs)
    }

    /// Check `commit` out into a standalone shallow repository fetched from
    /// the owner's checkout, replacing a leftover of the same run. Its Git
    /// metadata lives inside it, unlike a linked worktree's, whose gitdir is
    /// in the owner's `.git`: the confined script resolves `HEAD` and
    /// `ls-files` from the one writable tree it is given, and never needs,
    /// or writes, the owner's repository, which the sandbox does not show
    /// when it sits under the `/tmp` the sandbox replaces.
    fn create_evidence_checkout(
        &self,
        checkout_id: &str,
        commit: &str,
    ) -> Result<PathBuf, OrbitError> {
        self.remove_evidence_checkout(checkout_id)?;
        let path = recovery_checkout_path(&self.paths().state_dir, checkout_id)?;
        std::fs::create_dir_all(&path).map_err(|error| {
            OrbitError::Execution(format!(
                "create evidence checkout {}: {error}",
                path.display()
            ))
        })?;
        let git_dir = format!("--git-dir={}", path.join(".git").display());
        let source = self.paths().repo_root.to_string_lossy().into_owned();
        // Every command after `init` names the new repository explicitly, so
        // a failed `init` can never fall through to an enclosing one.
        for args in [
            vec!["init", "--quiet", "--template="],
            vec![
                &git_dir,
                "fetch",
                "--quiet",
                "--depth=1",
                "--no-tags",
                "--end-of-options",
                &source,
                commit,
            ],
            vec![
                &git_dir,
                "-c",
                "core.hooksPath=/dev/null",
                "checkout",
                "--quiet",
                "--detach",
                commit,
            ],
        ] {
            let output = run_git(&path, &args)?;
            if !output.success {
                return Err(OrbitError::Execution(format!(
                    "prepare evidence checkout {} at {commit}: git {}: {}",
                    path.display(),
                    args.join(" "),
                    output.stderr.trim()
                )));
            }
        }
        path.canonicalize().map_err(|error| {
            OrbitError::Execution(format!(
                "resolve evidence checkout {}: {error}",
                path.display()
            ))
        })
    }

    /// Remove a fulfilment run's checkout, if it exists. It is the run's own
    /// directory, registered nowhere else.
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

    /// Run one admitted CodeQL command in `checkout` and judge its result.
    fn run_codeql(&self, checkout: &Path, requirement: &ReviewEvidenceRequirement) -> CodeqlRun {
        let mut run = CodeqlRun {
            requirement: requirement.clone(),
            exit_code: None,
            timed_out: false,
            stdout: String::new(),
            stderr: String::new(),
            sarif: None,
            refusal: None,
            detail: String::new(),
        };
        let args = match admitted_codeql_args(&requirement.command) {
            Ok(args) => args,
            Err(detail) => {
                run.refusal = Some(FulfilmentRefusal::CommandNotAllowed);
                run.detail = detail;
                return run;
            }
        };
        let scratch = checkout.join(".orbit/tmp");
        if let Err(error) = std::fs::create_dir_all(&scratch) {
            run.refusal = Some(FulfilmentRefusal::CommandFailed);
            run.detail = format!("prepare scratch {}: {error}", scratch.display());
            return run;
        }
        let mut env = self.validation_environment().env;
        env.retain(|(name, _)| name != "ORBIT_SCRATCH_DIR");
        env.push((
            "ORBIT_SCRATCH_DIR".to_string(),
            scratch.to_string_lossy().into_owned(),
        ));
        #[cfg(target_os = "linux")]
        let outcome = {
            let request = ExecRequest {
                program: checkout.join(CODEQL_SCRIPT).to_string_lossy().into_owned(),
                args,
                current_dir: Some(checkout.to_string_lossy().into_owned()),
                timeout_ms: Some(CODEQL_TIMEOUT_MS),
                stdin_mode: StdinMode::Null,
                environment_mode: EnvironmentMode::ClearAndSet(env),
                debug: false,
            };
            run_process(&request, &EvidenceCodeqlSandbox::new(checkout))
        };
        #[cfg(not(target_os = "linux"))]
        let outcome: Result<orbit_types::tool::ExecutionResult, OrbitError> = {
            let _ = (args, env);
            Err(OrbitError::PolicyDenied(
                "CodeQL fulfilment requires Linux Bubblewrap".to_string(),
            ))
        };
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                run.refusal = Some(FulfilmentRefusal::CommandFailed);
                run.detail = error.to_string();
                return run;
            }
        };
        run.exit_code = outcome.exit_code;
        run.timed_out = outcome.timed_out;
        run.stdout = tail(&outcome.stdout);
        run.stderr = tail(&outcome.stderr);
        let output = format!("{}\n{}", outcome.stdout, outcome.stderr);
        let problem = |pattern: &str| {
            output
                .lines()
                .find(|line| line.contains(pattern))
                .unwrap_or_default()
                .trim()
                .to_string()
        };
        let judged = if outcome.timed_out {
            Err((
                FulfilmentRefusal::TimedOut,
                format!("exceeded {CODEQL_TIMEOUT_MS} ms"),
            ))
        } else if outcome.exit_code == Some(3) {
            Err((
                FulfilmentRefusal::PlatformRefused,
                problem("codeql-rust-local:"),
            ))
        } else if !outcome.success && output.contains("is required on PATH") {
            Err((
                FulfilmentRefusal::ToolMissing,
                problem("is required on PATH"),
            ))
        } else if !outcome.success && output.contains("incomplete Rust extraction") {
            Err((
                FulfilmentRefusal::AnalysisIncomplete,
                problem("incomplete Rust extraction"),
            ))
        } else if !outcome.success {
            Err((
                FulfilmentRefusal::CommandFailed,
                format!(
                    "exit {:?}: {}",
                    outcome.exit_code,
                    problem("codeql-rust-local:")
                ),
            ))
        } else if !output.contains(ANALYSIS_COMPLETED_MARKER) {
            Err((
                FulfilmentRefusal::AnalysisIncomplete,
                "the run exited zero without reporting completed analysis".to_string(),
            ))
        } else {
            judge_sarif(checkout, &output)
        };
        match judged {
            Ok(sarif) => run.sarif = Some(sarif),
            Err((refusal, detail)) => {
                run.refusal = Some(refusal);
                run.detail = detail;
            }
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
        runs: &[CodeqlRun],
        refusal: Option<FulfilmentRefusal>,
        detail: &str,
    ) -> Result<(), OrbitError> {
        let mut artifacts = Vec::new();
        for run in runs {
            // Check both final store keys at the system-authority write boundary.
            // Build the whole batch first, so a refusal cannot partially write it.
            let (artifact, log_artifact) = fulfilment_artifact_paths(&run.requirement.artifact)?;
            let log = json!({
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
                "sarif": run.sarif,
                "stdout": run.stdout,
                "stderr": run.stderr,
                "host_os": std::env::consts::OS,
                "recorded_at": Utc::now().to_rfc3339(),
            });
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
                };
                artifacts.push(json_artifact(&artifact, &evidence)?);
            }
        }
        let comment = match refusal {
            None => format!(
                "Owner evidence fulfilment run={run_id} ran every named CodeQL check at held \
                 candidate `{}` on this Linux host and attached the results and logs. A fresh \
                 review verifies them; nothing was approved.",
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

/// The hold on `task` this owner can fulfil: current, every requirement kind
/// `codeql`, and its evidence not yet arrived.
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
        || hold
            .requirements
            .iter()
            .any(|requirement| requirement.kind != ReviewEvidenceKind::CodeQl)
        || !hold_is_current(runtime, task, &hold)?
        || evidence_ready(runtime, &task.id, &hold)?
    {
        return Ok(None);
    }
    Ok(Some(hold))
}

/// One hold's identity: its attempt and exact candidate.
fn hold_key(hold: &ReviewEvidenceHold) -> String {
    format!("{}:{}", hold.attempt_id, hold.candidate.commit)
}

fn run_input_field<'a>(run: &'a JobRun, field: &str) -> Option<&'a str> {
    run.input.as_ref()?.get(field)?.as_str()
}

/// The script's arguments when `command` is exactly the CodeQL script with
/// its own options and one query selector. Every word is restricted to
/// characters with no shell meaning, so no hold can name another program.
fn admitted_codeql_args(command: &str) -> Result<Vec<String>, String> {
    let refuse = |why: &str| {
        Err(format!(
            "`{command}` is not an admitted CodeQL command: {why}"
        ))
    };
    let plain = |word: &str| {
        !word.is_empty()
            && word.chars().all(|c| {
                c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | ':' | '@' | '+' | '-')
            })
    };
    if command.chars().any(|c| c.is_whitespace() && c != ' ') {
        return refuse("only single spaces may separate words");
    }
    let mut words = command.split(' ').filter(|word| !word.is_empty());
    if words.next() != Some(CODEQL_SCRIPT) {
        return refuse(&format!("it must run `{CODEQL_SCRIPT}`"));
    }
    let mut args = Vec::new();
    let mut selector = false;
    while let Some(word) = words.next() {
        if !plain(word) {
            return refuse(&format!(
                "`{word}` has characters outside [A-Za-z0-9._/:@+-]"
            ));
        }
        match word {
            "--ram" | "--toolchain" => {
                let Some(value) = words
                    .next()
                    .filter(|value| plain(value) && !value.starts_with('-'))
                else {
                    return refuse(&format!("`{word}` needs a value"));
                };
                args.push(word.to_string());
                args.push(value.to_string());
            }
            _ if word.starts_with('-') => {
                return refuse(&format!("option `{word}` is not admitted"));
            }
            _ if selector => return refuse("it names more than one query selector"),
            _ => {
                selector = true;
                args.push(word.to_string());
            }
        }
    }
    if !selector {
        return refuse("it names no query selector");
    }
    Ok(args)
}

/// The run's SARIF, from the run directory the script printed inside
/// `checkout`. Any result is refused: a fresh review must judge findings.
fn judge_sarif(checkout: &Path, output: &str) -> Result<Value, (FulfilmentRefusal, String)> {
    let incomplete = |detail: String| (FulfilmentRefusal::AnalysisIncomplete, detail);
    let run_dir = output
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(RUN_DIRECTORY_MARKER))
        .map(|path| PathBuf::from(path.trim()))
        .ok_or_else(|| incomplete("the run printed no run directory".to_string()))?;
    let canonical_checkout = checkout
        .canonicalize()
        .map_err(|error| incomplete(error.to_string()))?;
    let sarif_path = run_dir
        .join("results.sarif")
        .canonicalize()
        .map_err(|error| {
            incomplete(format!(
                "results.sarif under {}: {error}",
                run_dir.display()
            ))
        })?;
    if !sarif_path.starts_with(&canonical_checkout) {
        return Err(incomplete(format!(
            "{} is outside the evidence checkout",
            sarif_path.display()
        )));
    }
    let unreadable = |detail: String| (FulfilmentRefusal::ResultsUnreadable, detail);
    let bytes = std::fs::read(&sarif_path).map_err(|error| unreadable(error.to_string()))?;
    let sarif: Value =
        serde_json::from_slice(&bytes).map_err(|error| unreadable(error.to_string()))?;
    let runs = sarif
        .get("runs")
        .and_then(Value::as_array)
        .filter(|runs| !runs.is_empty())
        .ok_or_else(|| unreadable("SARIF has no runs".to_string()))?;
    let mut results = 0usize;
    let mut rules = std::collections::BTreeSet::new();
    for run in runs {
        let found = run
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| unreadable("a SARIF run has no results array".to_string()))?;
        results += found.len();
        rules.extend(
            found
                .iter()
                .filter_map(|result| result.get("ruleId").and_then(Value::as_str))
                .map(str::to_string),
        );
    }
    let summary = json!({"runs": runs.len(), "results": results, "rules": rules});
    if results > 0 {
        return Err((
            FulfilmentRefusal::FindingsReported,
            format!(
                "{results} result(s) for rule(s) {}",
                rules.into_iter().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    Ok(summary)
}

/// `evidence/x.json` logs to `evidence/x.log.json`.
fn log_artifact_path(artifact: &str) -> String {
    match artifact.strip_suffix(".json") {
        Some(stem) => format!("{stem}.log.json"),
        None => format!("{artifact}.log.json"),
    }
}

fn fulfilment_artifact_paths(artifact: &str) -> Result<(String, String), OrbitError> {
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

/// The last [`MAX_CAPTURED_STREAM_BYTES`] of a stream.
fn tail(stream: &str) -> String {
    if stream.len() <= MAX_CAPTURED_STREAM_BYTES {
        return stream.to_string();
    }
    let start = ceil_char_boundary(stream, stream.len() - MAX_CAPTURED_STREAM_BYTES);
    format!("[… truncated]\n{}", &stream[start..])
}
