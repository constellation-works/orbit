//! Cancelling a run only while it is still queued.
//!
//! The dashboard's queued-window Cancel promises that the window has not
//! started and has admitted nothing. A window queued behind a live one starts
//! under the same run id when its predecessor ends, so a plain cancel sent
//! from a card rendered earlier would stop the window that just started.
//! This cancel decides "still pending" in the store transaction that starts
//! runs, before it records an audit, writes a task policy or signals anyone:
//! a run that started first is refused and left exactly as it was.

use chrono::Utc;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{PendingJobRunCancellation, TaskReservationReleaseReason};
use orbit_types::record::OrbitEvent;
use orbit_types::workflow::{JobRunState, RunCancellationProvenance, TaskCancellationPolicy};

use super::actions::{CancellationRequest, cancellation_note, cancellation_result};
use super::types::{JobRunCancelOutcome, JobRunCancelResult};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Cancel `run_id` only if it is still queued (`pending`).
    ///
    /// A run that already ended reports `already_terminal`, as
    /// [`Self::cancel_job_run_with_options_and_policy`] does. A run that has
    /// started is refused with [`OrbitError::JobRunStateTransition`] and
    /// nothing is recorded or signalled; cancelling a running run stays the
    /// general cancel's job. `block_task` selects the coupled tasks'
    /// disposition, as it does there.
    pub fn cancel_queued_job_run(
        &self,
        run_id: &str,
        actor: &str,
        source: &str,
        reason: Option<&str>,
        block_task: bool,
    ) -> Result<JobRunCancelResult, OrbitError> {
        let request = CancellationRequest {
            actor,
            source,
            reason: reason.map(str::trim).filter(|reason| !reason.is_empty()),
            block_task,
            keep_candidate: false,
        };
        let finished_at = Utc::now();
        match self
            .stores()
            .jobs()
            .cancel_pending_job_run(run_id, finished_at)?
        {
            PendingJobRunCancellation::Missing => Err(OrbitError::not_found(
                NotFoundKind::JobRun,
                run_id.to_string(),
            )),
            PendingJobRunCancellation::NotPending(state) if state.is_terminal() => self
                .cancel_job_run_with_reason_and_policy(
                    run_id,
                    actor,
                    source,
                    request.reason,
                    block_task,
                ),
            PendingJobRunCancellation::NotPending(state) => {
                Err(OrbitError::JobRunStateTransition(format!(
                    "job run '{run_id}' is no longer queued (it is {state}); \
                     the queued-only cancel was refused and the run left untouched"
                )))
            }
            PendingJobRunCancellation::Cancelled => {
                self.complete_queued_cancel(run_id, request, finished_at)
            }
        }
    }

    /// The general cancel's bookkeeping for a run this request alone
    /// cancelled while it was pending. Nothing was running, so nothing is
    /// signalled; the task policy is persisted before the coupled tasks are
    /// cleaned up, which reads it.
    fn complete_queued_cancel(
        &self,
        run_id: &str,
        request: CancellationRequest<'_>,
        finished_at: chrono::DateTime<Utc>,
    ) -> Result<JobRunCancelResult, OrbitError> {
        let cancelled = self
            .get_job_run_backend(run_id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::JobRun, run_id.to_string()))?;
        let mut queued = cancelled.clone();
        queued.state = JobRunState::Pending;
        queued.finished_at = None;
        let request_id = audit_execution_id("cancel");
        self.record_cancellation_request(
            &queued,
            &request_id,
            request.actor,
            request.source,
            request.reason,
            request.block_task,
        )?;
        let provenance =
            RunCancellationProvenance::new(request.actor, request.source, request.reason);
        let diagnostic = cancellation_note(request.actor, request.reason);
        self.persist_task_cancellation_policy(
            &cancelled,
            TaskCancellationPolicy {
                block: request.block_task,
                note: cancellation_note(&provenance.actor, provenance.reason.as_deref()),
                provenance: Some(provenance),
            },
        )?;
        self.cleanup_after_job_run_finalized(
            run_id,
            JobRunState::Cancelled,
            finished_at,
            TaskReservationReleaseReason::RunTerminal,
            Some(("RUN_CANCELLED", &diagnostic)),
        );
        self.record_run_cancelled_audit(
            &cancelled,
            &request_id,
            request.actor,
            request.source,
            request.reason,
            JobRunState::Pending,
        )?;
        self.mark_cancelled_pipeline_state(&cancelled)?;
        self.settle_child_dispatches_on_cancel(&cancelled, request, 0)?;
        self.record_event(OrbitEvent::JobRunCancelled {
            job_id: cancelled.job_id.clone(),
            run_id: run_id.to_string(),
            previous_state: Some(JobRunState::Pending.to_string()),
            final_state: Some(JobRunState::Cancelled.to_string()),
            actor: Some(request.actor.to_string()),
            source: Some(request.source.to_string()),
            signal_attempted: Some(false),
            signal_outcome: None,
        })?;
        self.record_cancellation_completion(&cancelled, &request_id, "cancelled", None, None)?;
        let mut result = cancellation_result(
            &queued,
            JobRunCancelOutcome::Cancelled,
            JobRunState::Cancelled,
            false,
            None,
            request.actor,
            request.source,
        );
        result.pull_settlements = self.pull_settlements_after_cancel(run_id);
        Ok(result)
    }
}
