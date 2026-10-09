//! Adjust the worker ceiling of a live drain [ORB-11253].
//!
//! Both drains carry one: an owner's `orbit run auto` window and a replica's
//! `orbit run auto --pull` drain each read the control below on every
//! admission pass. It is the only ceiling on their leaves — the leaf job
//! definitions declare no active-run limit of their own [ORB-13893].
//!
//! A drain's `max_active_leaf_runs` is snapshotted into the run's immutable
//! `initial_input` at submission, so raising it used to mean cancelling the
//! coordinator and submitting a replacement — which changes the run id, the
//! deadline, and the completion authorization, and leaves the already
//! dispatched children orphaned from the run that started them. This is the
//! supported alternative: one durable, audited control on the run's own
//! pipeline state that the admission path prefers over the submitted value.
//!
//! What it deliberately does not do is touch anything else about the run.
//! Lowering the ceiling stops *new* admissions until enough children finish;
//! it never signals, cancels, or reassigns a child, because a running leaf is
//! held by its own detached run, not by this ceiling.

use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{DrainWorkerLimit, JobRun, JobRunState, PipelineState, RunStateUpdate};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::distributed::PULL_DRAIN_JOB;
use crate::application::workflow::{AUTO_WORKFLOW_ALIAS, find_workflow};

const WORKER_LIMIT_REQUEST_AUDIT: &str = "pipeline.run.workers.requested";
const WORKER_LIMIT_COMPLETION_AUDIT: &str = "pipeline.run.workers.completed";

/// The governed-operation id this control is authorized under: the
/// `resize` action of the auto-drain control.
const WORKER_LIMIT_OPERATION: &str = "orbit.workflow.auto";

/// One operator request to move a live drain's worker ceiling.
#[derive(Debug, Clone, Copy)]
pub struct DrainWorkerLimitRequest<'a> {
    pub run_id: &'a str,
    pub max_active_leaf_runs: u32,
    /// Optional compare-and-set against
    /// [`PipelineState::drain_worker_limit_revision`]. Supplied, a caller that
    /// read one ceiling and computed another from it fails with
    /// [`OrbitError::JobRunControlConflict`] rather than overwriting a change
    /// that landed in between. Omitted, the write is last-writer-wins, which
    /// is what an operator typing an absolute number means.
    pub expected_revision: Option<u32>,
    pub reason: Option<&'a str>,
    pub actor: &'a str,
    /// Surface that made the request, recorded on the audit trail.
    pub source: &'a str,
    pub claim_token: Option<&'a str>,
}

/// Outcome of an accepted worker-ceiling change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainWorkerLimitChange {
    pub run_id: String,
    pub job_id: String,
    /// `updated` when the ceiling moved, `unchanged` when the requested value
    /// already was the effective one. Both are successes and both are audited;
    /// they differ in whether a revision was consumed.
    pub outcome: &'static str,
    pub previous_max_active_leaf_runs: u32,
    pub max_active_leaf_runs: u32,
    pub revision: u32,
}

impl OrbitRuntime {
    /// Set the live worker ceiling of an auto or pull drain that is still
    /// running.
    ///
    /// The liveness check and the write share one transaction, so a run that
    /// terminalizes concurrently refuses the update instead of accepting a
    /// control that nothing will ever read. Every outcome — accepted, refused,
    /// or lost to a concurrent update — is audited against the run.
    pub fn set_drain_worker_limit(
        &self,
        request: DrainWorkerLimitRequest<'_>,
    ) -> Result<DrainWorkerLimitChange, OrbitError> {
        self.require_workspace_claim(WORKER_LIMIT_OPERATION, request.claim_token)?;
        let request_id = audit_execution_id("workers");
        let outcome = self.apply_drain_worker_limit(request, &request_id);
        match &outcome {
            Ok(change) => self.record_worker_limit_completion(
                request.run_id,
                &request_id,
                change.outcome,
                json!({
                    "previous_max_active_leaf_runs": change.previous_max_active_leaf_runs,
                    "max_active_leaf_runs": change.max_active_leaf_runs,
                    "revision": change.revision,
                }),
                None,
            )?,
            Err(error) => self.record_worker_limit_completion(
                request.run_id,
                &request_id,
                "rejected",
                json!({ "requested_max_active_leaf_runs": request.max_active_leaf_runs }),
                Some(error.to_string()),
            )?,
        }
        outcome
    }

    fn apply_drain_worker_limit(
        &self,
        request: DrainWorkerLimitRequest<'_>,
        request_id: &str,
    ) -> Result<DrainWorkerLimitChange, OrbitError> {
        let run_id = request.run_id;
        let requested = request.max_active_leaf_runs;
        let run = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        let drain_jobs = drain_job_ids()?;
        if !drain_jobs.contains(&run.job_id.as_str()) {
            return Err(OrbitError::InvalidInput(format!(
                "job run '{run_id}' is a `{}` run; the worker ceiling is a `{}` or `{}` control",
                run.job_id, drain_jobs[0], drain_jobs[1]
            )));
        }
        if requested == 0 {
            return Err(OrbitError::InvalidInput(
                "worker ceiling must be at least 1".to_string(),
            ));
        }
        if run.state.is_terminal() {
            return Err(terminal_run_error(run_id, run.state));
        }
        let submitted = self.submitted_max_active_leaf_runs(&run)?;
        self.record_worker_limit_request(&run, request_id, request)?;

        let mut applied: Option<DrainWorkerLimit> = None;
        let mut effective_before_request = submitted;
        let mut unchanged = false;
        let update = self.stores().jobs().update_run_state(
            run_id,
            &mut |run_state: JobRunState, state: &mut PipelineState| {
                // Re-checked inside the write transaction: the run's own worker
                // can terminalize it between the read above and this write, and
                // a control nothing will ever read is not a success.
                if run_state.is_terminal() {
                    return Err(terminal_run_error(run_id, run_state));
                }
                effective_before_request = state.effective_max_active_leaf_runs(submitted);
                let revision = state.drain_worker_limit_revision();
                if request
                    .expected_revision
                    .is_some_and(|expected| expected != revision)
                {
                    return Err(revision_conflict(
                        run_id,
                        request.expected_revision,
                        revision,
                    ));
                }
                if state.effective_max_active_leaf_runs(submitted) == requested {
                    // Idempotent: re-issuing the ceiling a drain already has
                    // must not consume a revision, or a retried request would
                    // invalidate the compare-and-set handle every other
                    // operator is holding.
                    unchanged = true;
                    applied = state.drain_worker_limit.clone();
                    return Ok(());
                }
                state.set_drain_worker_limit(
                    requested,
                    submitted,
                    request.actor.to_string(),
                    request.reason.map(str::to_string),
                    request.expected_revision,
                );
                applied = state.drain_worker_limit.clone();
                Ok(())
            },
        )?;

        match update {
            RunStateUpdate::Updated => {}
            RunStateUpdate::NotFound => {
                return Err(OrbitError::not_found(
                    NotFoundKind::JobRun,
                    run_id.to_string(),
                ));
            }
            // A submitted-but-unstarted drain has no checkpoint to carry the
            // control yet. Refusing is the honest answer: the run would read
            // its submitted input and silently ignore the adjustment.
            RunStateUpdate::NoState => {
                return Err(OrbitError::JobValidation(format!(
                    "job run '{run_id}' has not started; resubmit it with the ceiling you want, or wait for it to begin"
                )));
            }
        }

        Ok(DrainWorkerLimitChange {
            run_id: run_id.to_string(),
            job_id: run.job_id,
            outcome: if unchanged { "unchanged" } else { "updated" },
            previous_max_active_leaf_runs: effective_before_request,
            max_active_leaf_runs: requested,
            revision: applied.as_ref().map_or(0, |limit| limit.revision),
        })
    }

    /// The drain a resize without an explicit run ID targets: the one auto or
    /// pull drain pending or running in this workspace. None running, or more
    /// than one, is refused rather than guessed, so the caller names the run.
    pub fn active_drain_run_id(&self) -> Result<String, OrbitError> {
        let mut active = Vec::new();
        for job_id in drain_job_ids()? {
            active.extend(
                self.stores()
                    .jobs()
                    .list_pending_or_running_job_runs(job_id)?,
            );
        }
        match active.len() {
            0 => Err(OrbitError::InvalidInput(
                "no auto or pull drain is pending or running in this workspace".to_string(),
            )),
            1 => Ok(active.remove(0).run_id),
            count => Err(OrbitError::InvalidInput(format!(
                "{count} drains are live in this workspace; pass `id` to choose one"
            ))),
        }
    }

    /// The ceiling this run was submitted with: its own input when it carries
    /// one, otherwise its drain job's declared default.
    pub(super) fn submitted_max_active_leaf_runs(&self, run: &JobRun) -> Result<u32, OrbitError> {
        if let Some(submitted) = run
            .input
            .as_ref()
            .and_then(|input| input.get("max_active_leaf_runs"))
            .and_then(job_input_u32)
        {
            return Ok(submitted);
        }
        Ok(self
            .resolved_job_spec(&run.job_id)?
            .default_input
            .as_ref()
            .and_then(|input| input.get("max_active_leaf_runs"))
            .and_then(job_input_u32)
            .unwrap_or(1))
    }

    fn record_worker_limit_request(
        &self,
        run: &JobRun,
        request_id: &str,
        request: DrainWorkerLimitRequest<'_>,
    ) -> Result<(), OrbitError> {
        self.record_pipeline_audit(
            WORKER_LIMIT_REQUEST_AUDIT,
            Some(&run.run_id),
            Some(request.actor),
            AuditEventStatus::Success,
            json!({
                "request_id": request_id,
                "run_id": run.run_id,
                "job_id": run.job_id,
                "observed_state": run.state.to_string(),
                "requested_max_active_leaf_runs": request.max_active_leaf_runs,
                "expected_revision": request.expected_revision,
                "reason": request.reason,
                "actor": request.actor,
                "source": request.source,
                "requested_at": chrono::Utc::now().to_rfc3339(),
            }),
            None,
        )
    }

    fn record_worker_limit_completion(
        &self,
        run_id: &str,
        request_id: &str,
        outcome: &str,
        detail: Value,
        error: Option<String>,
    ) -> Result<(), OrbitError> {
        let mut arguments = json!({
            "request_id": request_id,
            "run_id": run_id,
            "outcome": outcome,
            "completed_at": chrono::Utc::now().to_rfc3339(),
        });
        if let (Some(target), Some(detail)) = (arguments.as_object_mut(), detail.as_object()) {
            for (key, value) in detail {
                target.insert(key.clone(), value.clone());
            }
        }
        self.record_pipeline_audit(
            WORKER_LIMIT_COMPLETION_AUDIT,
            Some(run_id),
            None,
            if outcome == "rejected" {
                AuditEventStatus::Failure
            } else {
                AuditEventStatus::Success
            },
            arguments,
            error,
        )
    }
}

/// The drain jobs that carry a worker ceiling: the owner's auto drain and a
/// replica's pull drain.
fn drain_job_ids() -> Result<[&'static str; 2], OrbitError> {
    let auto = find_workflow(AUTO_WORKFLOW_ALIAS)
        .map(|workflow| workflow.job_id)
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!("unknown workflow '{AUTO_WORKFLOW_ALIAS}'"))
        })?;
    Ok([auto, PULL_DRAIN_JOB])
}

/// A run input value that went through the template engine may arrive as a
/// string; accept both, exactly as the admission path does.
fn job_input_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|value| u32::try_from(value).ok()),
        Value::String(text) => text.trim().parse::<u32>().ok(),
        _ => None,
    }
}

fn terminal_run_error(run_id: &str, state: JobRunState) -> OrbitError {
    OrbitError::JobValidation(format!(
        "job run '{run_id}' is {state}; a terminal run admits no further work"
    ))
}

fn revision_conflict(run_id: &str, expected: Option<u32>, actual: u32) -> OrbitError {
    OrbitError::JobRunControlConflict(format!(
        "worker ceiling of job run '{run_id}' is at revision {actual}, not {}; re-read it and decide again",
        expected.unwrap_or(actual)
    ))
}
