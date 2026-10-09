//! Gating and the clock tick that dispatches fulfilment runs.

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::{
    EvidenceHostOs, JobRun, JobRunState, JobRunTrigger, ReviewEvidenceHold, ReviewEvidenceKind,
    ReviewEvidenceRequirement,
};
use serde_json::{Value, json};

use super::super::evidence::{evidence_hold, evidence_ready, hold_is_current};
use crate::OrbitRuntime;
use crate::application::job::pipeline::{
    PipelineSubmission, ROUTINE_DISPATCH_ORBIT_DIR_FIELD, RetryKey,
};
use crate::application::task::SYSTEM_ACTOR_LABEL;

use super::{
    ATTEMPT_KEY_FIELD, DEFAULT_FULFILMENT_MIN_FREE_MIB, FULFIL_STEP, FulfilmentRefusal,
    HOLD_KEY_FIELD, MAX_ACTIVE_EVIDENCE_FULFILMENTS, MAX_FULFILMENT_ATTEMPTS,
    REVIEW_EVIDENCE_FULFILMENT_JOB, RUN_SCAN_LIMIT, TRIGGER_CONSUMER, TRIGGER_NAME,
};

/// What one fulfilment tick did.
#[derive(Debug, Default)]
pub struct EvidenceFulfilmentTick {
    /// Why the tick did nothing, when it stood down.
    pub skipped: Option<String>,
    /// `(task id, run id)` for each fulfilment it started.
    pub dispatched: Vec<(String, String)>,
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
    pub(super) fn fulfilment_owner_refusal(&self) -> Option<(FulfilmentRefusal, String)> {
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
    pub(super) fn fulfilment_host_refusal(&self) -> Option<(FulfilmentRefusal, String)> {
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
    /// this host can produce, at most `MAX_ACTIVE_EVIDENCE_FULFILMENTS`
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
}

/// The hold on `task` this owner can fulfil: current, every requirement a
/// `codeql` run or a Linux `host_sandbox_test`, and its evidence not yet
/// arrived.
pub(super) fn fulfilable_hold(
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

/// One hold's identity: its attempt and exact candidate.
pub(super) fn hold_key(hold: &ReviewEvidenceHold) -> String {
    format!("{}:{}", hold.attempt_id, hold.candidate.commit)
}

fn run_input_field<'a>(run: &'a JobRun, field: &str) -> Option<&'a str> {
    run.input.as_ref()?.get(field)?.as_str()
}

/// A `min_free_mib` input field, rendered as a number or numeric string.
pub(super) fn min_free_mib_of(input: &Value) -> Option<u64> {
    match input.get("min_free_mib")? {
        Value::String(text) => text.trim().parse().ok(),
        value => value.as_u64(),
    }
}

pub(super) fn free_mib(path: &Path) -> Result<u64, OrbitError> {
    fs2::available_space(path)
        .map(|bytes| bytes / (1024 * 1024))
        .map_err(|error| OrbitError::Io(format!("free space under {}: {error}", path.display())))
}
