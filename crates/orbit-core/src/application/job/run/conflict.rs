//! A run's terminal write and the cleanup it owes, and [ORB-10597]
//! terminal-outcome conflict recording.
//!
//! `finalize_job_run_with_reservation_cleanup` writes the terminal state once,
//! releases the run's task reservations, couples its tasks out
//! (`block_on_run_failure`), records a claimed leaf's settlement and releases
//! the review attempts the run still holds. The rest of this header is about
//! the one case that write cannot settle on its own.
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
use orbit_store::contract::{JobRunFinalization, JobRunStepParams, TaskReservationReleaseReason};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{JobRunState, JobTargetType};
use serde_json::json;

use crate::OrbitRuntime;
use crate::application::job::{JobPullSettlementPort, ReviewAdmissionHook};

/// `error_code` stamped on the diagnostic step recording a conflicting terminal
/// outcome. Also the step's dedupe key: the conflict is recorded once per run,
/// however many times the losing outcome is re-delivered.
pub(crate) const TERMINAL_OUTCOME_CONFLICT_CODE: &str = "terminal_outcome_conflict";

/// Audit tool name the same conflict is filed under.
const TERMINAL_OUTCOME_CONFLICT_AUDIT: &str = "pipeline.run.terminal_conflict";

impl OrbitRuntime {
    pub fn finalize_job_run_with_reservation_cleanup(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
        release_reason: TaskReservationReleaseReason,
    ) -> Result<bool, OrbitError> {
        self.finalize_job_run_with_reservation_cleanup_and_diagnostic(
            run_id,
            state,
            finished_at,
            duration_ms,
            release_reason,
            None,
        )
    }

    /// [`Self::finalize_job_run_with_reservation_cleanup`] for a caller that
    /// records its `(error_code, message)` diagnostic step only after the
    /// terminal write — orphan reconciliation, which must not leave a step on
    /// a run a live worker completed first. The diagnostic goes straight into
    /// the coupled tasks' block note instead of being read back from steps.
    pub fn finalize_job_run_with_reservation_cleanup_and_diagnostic(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
        release_reason: TaskReservationReleaseReason,
        diagnostic: Option<(&str, &str)>,
    ) -> Result<bool, OrbitError> {
        let outcome = self.stores().jobs().finalize_job_run_with_outcome(
            run_id,
            state,
            finished_at,
            duration_ms,
        )?;
        // [ORB-10597] The store keeps the first terminal state and drops this
        // one. An identical re-finalization is an ordinary idempotent replay; a
        // *different* terminal outcome is a real contradiction — most sharply,
        // a run condemned to `interrupted` while it was still working and then
        // reporting the success it actually reached. That must not vanish, so
        // it is recorded on the run instead of discarded. See
        // this module's header for why the recorded state still wins.
        // A cancellation request that loses to a worker's terminal outcome is
        // reported to the caller as `already_terminal`; it did not produce a
        // second durable outcome. The inverse remains a real contradiction: a
        // worker reporting success/failure after `cancelled` is still recorded.
        if let JobRunFinalization::AlreadyTerminal(prior) = outcome
            && prior != state
            && state != JobRunState::Cancelled
        {
            self.record_terminal_outcome_conflict(run_id, prior, state, finished_at);
        }
        if state.is_terminal() {
            self.cleanup_after_terminal_write(
                run_id,
                state,
                outcome,
                finished_at,
                release_reason,
                diagnostic,
            );
        }
        Ok(outcome != JobRunFinalization::Missing)
    }

    /// The cleanup a run's terminal write owes, for a caller whose own atomic
    /// store decision just terminalized the run into `state`, such as a
    /// pending-only cancel: the same release, coupled-task and settlement
    /// steps [`Self::finalize_job_run_with_reservation_cleanup`] runs after a
    /// `Finalized` outcome.
    pub fn cleanup_after_job_run_finalized(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        release_reason: TaskReservationReleaseReason,
        diagnostic: Option<(&str, &str)>,
    ) {
        self.cleanup_after_terminal_write(
            run_id,
            state,
            JobRunFinalization::Finalized,
            finished_at,
            release_reason,
            diagnostic,
        );
    }

    fn cleanup_after_terminal_write(
        &self,
        run_id: &str,
        state: JobRunState,
        outcome: JobRunFinalization,
        finished_at: DateTime<Utc>,
        release_reason: TaskReservationReleaseReason,
        diagnostic: Option<(&str, &str)>,
    ) {
        self.best_effort_release_task_reservations_for_owner_run_id(run_id, release_reason);
        // Coupling-out: clean up the run's coupled tasks only on the first
        // terminalization into a failure, cancellation, or interrupted state. Gating on
        // the atomic `Finalized` outcome keeps this idempotent — a replayed
        // terminalization is a no-op and never clobbers a task a human
        // already moved on. Cancellation selects backlog versus blocked.
        // Cleanup is best-effort so a status-write failure never blocks
        // the run from terminalizing or its reservations/file locks from being released.
        if outcome == JobRunFinalization::Finalized
            && super::block_on_run_failure::run_state_blocks_coupled_tasks(state)
        {
            self.best_effort_block_tasks_for_terminal_run(run_id, state, diagnostic);
        }
        // [ORB-13663] A claimed leaf's settlement is recorded as it
        // terminalizes, whichever process terminalizes it, and its own
        // worker delivers it — no longer only the drain that admitted it.
        // Not gated on `Finalized`: recording and delivery are idempotent,
        // and a replay retries a delivery an earlier attempt lost.
        JobPullSettlementPort::settle_terminalized_leaf(self, run_id, diagnostic);
        // [ORB-13890] A run never ends holding a review attempt open.
        // Idempotent, so not gated on `Finalized` either.
        ReviewAdmissionHook::release_run_review_attempts(self, run_id, finished_at);
    }
}

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
                lock_wait_ms: None,
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
