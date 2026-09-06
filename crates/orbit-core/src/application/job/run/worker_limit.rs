//! Adjust the worker ceiling of a live auto drain [ORB-11253].
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
use crate::application::workflow::{AUTO_WORKFLOW_ALIAS, SHIP_WORKFLOW_ALIAS, find_workflow};

const WORKER_LIMIT_REQUEST_AUDIT: &str = "pipeline.run.workers.requested";
const WORKER_LIMIT_COMPLETION_AUDIT: &str = "pipeline.run.workers.completed";

/// The governed-operation id this control is authorized under.
const WORKER_LIMIT_OPERATION: &str = "orbit.workflow.run.workers";

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
    /// The ceiling `task_auto_pipeline`'s own `max_active_runs` imposes above
    /// this one, echoed so an operator sees the headroom they have left.
    pub hard_limit: u32,
}

impl OrbitRuntime {
    /// Set the live worker ceiling of an auto drain that is still running.
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
                    "hard_limit": change.hard_limit,
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
        let drain_job_id = workflow_job_id(AUTO_WORKFLOW_ALIAS)?;
        if run.job_id != drain_job_id {
            return Err(OrbitError::InvalidInput(format!(
                "job run '{run_id}' is a `{}` run; the worker ceiling is a `{drain_job_id}` control",
                run.job_id
            )));
        }
        let hard_limit = self.leaf_run_hard_limit()?;
        if !(1..=hard_limit).contains(&requested) {
            return Err(OrbitError::InvalidInput(format!(
                "worker ceiling must be between 1 and {hard_limit}, the `{}` job's own active-run limit",
                workflow_job_id(SHIP_WORKFLOW_ALIAS)?
            )));
        }
        if run.state.is_terminal() {
            return Err(terminal_run_error(run_id, run.state));
        }
        let submitted = self.submitted_max_active_leaf_runs(&run)?;
        self.record_worker_limit_request(&run, request_id, request)?;

        let mut applied: Option<DrainWorkerLimit> = None;
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
            previous_max_active_leaf_runs: applied
                .as_ref()
                .map_or(submitted, |limit| limit.previous_max_active_leaf_runs),
            max_active_leaf_runs: requested,
            revision: applied.as_ref().map_or(0, |limit| limit.revision),
            hard_limit,
        })
    }

    /// The ceiling the leaf job imposes above the drain's own: no drain can
    /// keep more `task_auto_pipeline` runs live than that job admits, so a
    /// larger number would be accepted and then silently queue.
    pub(crate) fn leaf_run_hard_limit(&self) -> Result<u32, OrbitError> {
        Ok(self
            .resolved_job_spec(workflow_job_id(SHIP_WORKFLOW_ALIAS)?)?
            .max_active_runs
            .max(1))
    }

    /// The ceiling this run was submitted with: its own input when it carries
    /// one, otherwise the drain job's declared default.
    fn submitted_max_active_leaf_runs(&self, run: &JobRun) -> Result<u32, OrbitError> {
        if let Some(submitted) = run
            .input
            .as_ref()
            .and_then(|input| input.get("max_active_leaf_runs"))
            .and_then(job_input_u32)
        {
            return Ok(submitted);
        }
        Ok(self
            .resolved_job_spec(workflow_job_id(AUTO_WORKFLOW_ALIAS)?)?
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

fn workflow_job_id(alias: &str) -> Result<&'static str, OrbitError> {
    find_workflow(alias)
        .map(|workflow| workflow.job_id)
        .ok_or_else(|| OrbitError::InvalidInput(format!("unknown workflow '{alias}'")))
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
