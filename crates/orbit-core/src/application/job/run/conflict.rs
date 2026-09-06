//! [ORB-10597] Terminal-outcome conflict recording.
//!
//! A run's terminal state is written once and never overwritten: `finalize_run`
//! returns early when the run is already terminal. That guard is correct — see
//! below — but on its own it made a *conflicting* second outcome indistinguishable
//! from an idempotent replay, so a run condemned to `interrupted` while it was
//! still working could run to a genuine success and have that success dropped
//! with no error, no warning, and nothing in the durable record. The run's
//! state, `finished_at`, and `duration_ms` then permanently contradicted its own
//! steps and audit trail.
//!
//! ## Why first-writer-wins on `state` is kept
//!
//! Letting the later outcome overwrite was rejected:
//!
//! - **The first terminalization already fired irreversible side effects.**
//!   Coupled tasks were blocked, task reservations and file locks released, the
//!   routine fire resolved. Flipping the run to `success` afterwards asserts a
//!   consistency that no longer holds, and nothing re-acquires what was released.
//! - **Cancellation must not be revocable.** `orbit job cancel` writes
//!   `Cancelled`; a worker that reports success moments later must not resurrect
//!   the run, or cancel becomes racy by construction.
//! - **Replay determinism.** Finalization is reachable from several paths
//!   (`exec`, the pipeline worker, reconcile, cancel). Last-writer-wins makes the
//!   persisted state depend on their interleaving.
//!
//! What was actually missing is not a winner rule but a *record*: the operator
//! needs to know the two outcomes disagreed. So the terminal state stands and
//! the conflicting outcome is recorded durably and loudly instead of dropped.
//! A cancellation request that loses to a terminal worker outcome is the one
//! exception: cancellation reports `already_terminal` and does not manufacture
//! a conflict by attempting to replace the worker's outcome. The inverse is
//! still a real conflict — a worker outcome reported after `cancelled` remains
//! explicit here.
//!
//! ## Where a reader sees it
//!
//! 1. A diagnostic step appended to the run, stamped `error_code =
//!    terminal_outcome_conflict`, naming both outcomes and the reported finish
//!    time. Visible wherever run steps are — `orbit run show <run_id>` and the
//!    dashboard run detail — right next to the state it contradicts.
//! 2. An audit row under the `pipeline.run.terminal_conflict` tool name,
//!    targeted at the run, carrying both states as structured arguments.
//! 3. A `tracing::warn!` on `orbit.core.job_run` for log-based alerting.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRunState, JobTargetType};
use serde_json::json;

use crate::OrbitRuntime;

/// `error_code` stamped on the diagnostic step recording a conflicting terminal
/// outcome. Also the step's dedupe key: the conflict is recorded once per run,
/// however many times the losing outcome is re-delivered.
pub(crate) const TERMINAL_OUTCOME_CONFLICT_CODE: &str = "terminal_outcome_conflict";

/// Audit tool name the same conflict is filed under.
const TERMINAL_OUTCOME_CONFLICT_AUDIT: &str = "pipeline.run.terminal_conflict";

impl OrbitRuntime {
    /// Record that `reported` arrived for a run already terminal in `recorded`.
    ///
    /// Best-effort by design: this is a diagnostic about a state the caller has
    /// already committed to, so a failure to write it must never turn a
    /// completed finalization into an error. Failures are logged and swallowed.
    ///
    /// Only conflicting states reach here — an identical re-finalization is an
    /// ordinary idempotent replay and is not a conflict.
    pub(crate) fn record_terminal_outcome_conflict(
        &self,
        run_id: &str,
        recorded: JobRunState,
        reported: JobRunState,
        reported_finished_at: DateTime<Utc>,
    ) {
        tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            recorded_state = %recorded,
            reported_state = %reported,
            reported_finished_at = %reported_finished_at.to_rfc3339(),
            "job run reported a terminal outcome conflicting with the one already \
             recorded; the recorded state stands and the conflict is preserved on the run",
        );

        if let Err(error) =
            self.persist_terminal_outcome_conflict(run_id, recorded, reported, reported_finished_at)
        {
            tracing::warn!(
                target: "orbit.core.job_run",
                run_id,
                error = %error,
                "failed to persist terminal outcome conflict record",
            );
        }
    }

    fn persist_terminal_outcome_conflict(
        &self,
        run_id: &str,
        recorded: JobRunState,
        reported: JobRunState,
        reported_finished_at: DateTime<Utc>,
    ) -> Result<(), OrbitError> {
        // `show_job_run` is the accessor that populates `steps` (they live in
        // their own table, not on the run row).
        let run = self.show_job_run(run_id)?;
        if run
            .steps
            .iter()
            .any(|step| step.error_code.as_deref() == Some(TERMINAL_OUTCOME_CONFLICT_CODE))
        {
            return Ok(());
        }

        let message = terminal_outcome_conflict_message(
            recorded,
            run.finished_at,
            reported,
            reported_finished_at,
        );
        let step_index = run
            .steps
            .iter()
            .map(|step| step.step_index)
            .max()
            .map(|index| index.saturating_add(1) as usize)
            .unwrap_or(0);
        self.stores().jobs().complete_job_run_step(
            run_id,
            &JobRunStepParams {
                step_index,
                target_type: JobTargetType::Job,
                target_id: run.job_id.clone(),
                started_at: reported_finished_at,
                finished_at: reported_finished_at,
                duration_ms: Some(0),
                exit_code: None,
                agent_response_json: None,
                // The step carries the *recorded* state: it documents the run as
                // it stands, not a state transition it did not cause.
                state: recorded,
                error_code: Some(TERMINAL_OUTCOME_CONFLICT_CODE.to_string()),
                error_message: Some(message.clone()),
            },
        )?;

        self.record_pipeline_audit(
            TERMINAL_OUTCOME_CONFLICT_AUDIT,
            Some(run_id),
            None,
            AuditEventStatus::Failure,
            json!({
                "run_id": run_id,
                "recorded_state": recorded.to_string(),
                "recorded_finished_at": run.finished_at.map(|at| at.to_rfc3339()),
                "reported_state": reported.to_string(),
                "reported_finished_at": reported_finished_at.to_rfc3339(),
            }),
            Some(message),
        )
    }
}

/// The human-readable conflict record. Names both outcomes and both finish
/// times, so a reader can tell which one the durable state reflects and what it
/// is contradicting.
pub(crate) fn terminal_outcome_conflict_message(
    recorded: JobRunState,
    recorded_finished_at: Option<DateTime<Utc>>,
    reported: JobRunState,
    reported_finished_at: DateTime<Utc>,
) -> String {
    format!(
        "conflicting terminal outcome: run was already recorded {} at {} and later reported {} at {}; \
         the recorded state stands (terminal state is written once) and the reported outcome was not applied",
        recorded,
        recorded_finished_at
            .map(|at| at.to_rfc3339())
            .unwrap_or_else(|| "-".to_string()),
        reported,
        reported_finished_at.to_rfc3339(),
    )
}
