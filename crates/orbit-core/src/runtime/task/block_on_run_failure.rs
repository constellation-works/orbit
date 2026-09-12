//! Task coupling-out on run terminalization: when a job run reaches a terminal
//! *failure* state (`failed`, `timeout`, `cancelled`), every task coupled to
//! that run — stamped with its `job_run_id` during `worktree_setup` — is moved
//! to `blocked` so a human/orchestrator has to look before anything runs again.
//!
//! This is the symmetric counterpart to the coupling-in that
//! `worktree_setup` performs (stamping `job_run_id` and moving tasks to
//! `in_progress`). It reuses the same `blocked_workflow_failure_update` helper
//! the legacy parallel-batch path already uses, so the status event
//! (`workflow_run_failed`) and note format stay consistent across paths.
//!
//! `blocked` is a deliberate dead end for automation: `Blocked` is not in the
//! workflow-admission allowlist, so the ship
//! sweep skips these tasks. The only way out is a human/orchestrator decision
//! (`orbit.task.update` with `status: in_progress`, which accepts `Blocked` and leaves the task
//! `in-progress` — a status workflow admission does accept — or moving it back
//! to backlog with `orbit task update <id> --status backlog`).

use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, blocked_workflow_failure_update};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{JobRun, JobRunState};

use crate::OrbitRuntime;

/// Terminal run states that strand a coupled task and therefore trigger the
/// block transition. `Interrupted` is intentionally excluded: an orphaned run
/// (`interrupted`) is resumable from its step checkpoints, so its task stays
/// `in_progress` for the resume rather than being blocked.
pub(crate) fn is_workflow_failure_state(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed | JobRunState::Timeout | JobRunState::Cancelled
    )
}

/// Task statuses that should NOT be touched when their run terminalizes as a
/// failure: `Done`/`Archived`/`Rejected` are terminal or human decisions,
/// `Review` was already shipped (don't clobber it), and `Blocked` is already
/// where we want it (keeps the transition idempotent).
///
/// [ORB-11305] `Proposed` and `Someday` join them. Both are withdrawals — the
/// way a human takes work back out of the backlog — and a withdrawal is
/// routinely what *causes* the run to be cancelled. Cleanup that ran after it
/// would replace the human's decision with `blocked`, so the withdrawal would
/// have to be re-applied by hand once the run finished unwinding. Only a task
/// that is still `in-progress` (or already `backlog`) under the failed run is
/// this transition's business.
fn task_is_blockable_on_run_failure(status: TaskStatus) -> bool {
    !matches!(
        status,
        TaskStatus::Done
            | TaskStatus::Review
            | TaskStatus::Blocked
            | TaskStatus::Rejected
            | TaskStatus::Archived
            | TaskStatus::Proposed
            | TaskStatus::Someday
    )
}

/// Extract `(error_code, error_message)` for the failure note from the run's
/// most recent errored step. The pipeline records a single job-level
/// diagnostic step whose message already names the failing step, so surfacing
/// it verbatim keeps the note actionable.
pub(crate) fn failed_run_error_context(run: &JobRun) -> (Option<String>, Option<String>) {
    run.steps
        .iter()
        .rev()
        .find(|step| step.error_code.is_some() || step.error_message.is_some())
        .map(|step| (step.error_code.clone(), step.error_message.clone()))
        .unwrap_or((None, None))
}

impl OrbitRuntime {
    /// Best-effort variant used from run terminalization: a status-write
    /// failure is logged and swallowed so the run still reaches its terminal
    /// state and releases its reservations/file locks.
    pub(crate) fn best_effort_block_tasks_for_failed_run(&self, run_id: &str, state: JobRunState) {
        if let Err(error) = self.block_tasks_for_failed_run(run_id) {
            tracing::warn!(
                run_id,
                state = %state,
                "failed to block tasks coupled to terminally-failed job run: {error}"
            );
        }
    }

    fn block_tasks_for_failed_run(&self, run_id: &str) -> Result<(), OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(());
        };
        let (error_code, error_message) = failed_run_error_context(&run);
        let tasks = self.list_tasks_filtered(None, None, None, Some(run_id), None, None)?;
        for task in tasks {
            if !task_is_blockable_on_run_failure(task.status) {
                continue;
            }
            let update = blocked_workflow_failure_update(
                &run.job_id,
                run_id,
                error_code.as_deref(),
                error_message.as_deref(),
            );
            // Per-task best-effort: one task's write failure must not strand the
            // rest of the bundle.
            if let Err(error) = self.apply_task_automation_update(&task.id, update) {
                tracing::warn!(
                    run_id,
                    task_id = %task.id,
                    "failed to block task coupled to terminally-failed job run: {error}"
                );
            }
        }
        Ok(())
    }
}
