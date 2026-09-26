//! Task coupling-out on run terminalization: when a job run reaches a terminal
//! *failure* state (`failed`, `timeout`, `cancelled`) or is reconciled
//! `interrupted`, every task coupled to that run — stamped with its
//! `job_run_id` during `worktree_setup` — is moved to `blocked` so a
//! human/orchestrator has to look before anything runs again.
//!
//! This is the symmetric counterpart to the coupling-in that
//! `worktree_setup` performs (stamping `job_run_id` and moving tasks to
//! `in_progress`). It reuses the same `blocked_workflow_failure_update` helper
//! the legacy parallel-batch path already uses, so the status event
//! (`workflow_run_failed`) and note format stay consistent across paths.
//!
//! [ORB-12969] An `interrupted` run blocks its tasks too, through
//! `blocked_workflow_interruption_update` (`workflow_run_interrupted`, same
//! note shape plus the resume command). Leaving them `in-progress` hid work
//! stranded by a host reboot behind a status identical to healthy in-flight
//! work. The distinct event tells an operator to resume rather than diagnose,
//! and resume re-admits the task (`application::job::resume`), so the block
//! costs the recovery path nothing.
//!
//! `blocked` is a deliberate dead end for automation: `Blocked` is not in the
//! workflow-admission allowlist, so the ship
//! sweep skips these tasks. The only way out is a human/orchestrator decision
//! (`orbit.task.update` with `status: in_progress`, which accepts `Blocked` and leaves the task
//! `in-progress` — a status workflow admission does accept — or moving it back
//! to backlog with `orbit task update <id> --status backlog`), or resuming the
//! run that blocked it.
//!
//! Some failures are the host's, not the task's: dispatch could not find the
//! provider launcher. That error is permanent for its run, but installing the
//! launcher clears it, and nothing else would ever re-evaluate the block.
//! [`OrbitRuntime::infra_blocked_tasks`] classifies those blocks from the
//! failure note and re-resolves the launcher now, so `orbit doctor` can report
//! cleared ones and `orbit task recheck-blocked --confirm` can return them to
//! backlog. Every other block keeps the human decision described above.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::activity_job::cli_runner::missing_launcher_in;
use orbit_engine::{
    RuntimeHost, WORKFLOW_RUN_FAILED_EVENT, blocked_workflow_failure_update,
    blocked_workflow_interruption_update,
};
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{JobRun, JobRunState};

use crate::OrbitRuntime;

/// A blocked task whose block was caused by host configuration — its run
/// failed because dispatch could not find the provider launcher — rather than
/// by the task's own work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfraBlockedTask {
    pub task_id: String,
    pub title: String,
    /// When the blocking history entry was recorded; identifies the block, so
    /// a requeue can confirm it is still the one it classified.
    pub blocked_at: DateTime<Utc>,
    pub run_id: Option<String>,
    /// Launcher program named by the failure, as the executor configured it.
    pub program: String,
    pub provider: String,
    /// Where dispatch from this workspace would launch `program` now, or
    /// `None` while the condition still reproduces.
    pub launcher: Option<PathBuf>,
}

/// Terminal run states that strand a coupled task and therefore trigger the
/// block transition. `Interrupted` is included [ORB-12969]: the run is
/// resumable from its step checkpoints, but nothing resumes it on its own, so
/// its task must not keep looking like live work. Resume re-admits the blocked
/// task, so blocking it does not get in the way of the resume.
pub(crate) fn run_state_blocks_coupled_tasks(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed
            | JobRunState::Timeout
            | JobRunState::Cancelled
            | JobRunState::Interrupted
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

/// The entry that put the task in its current block, when that entry is a
/// workflow failure naming a missing provider launcher. A later block of any
/// other kind (an operator's, another run's failure) supersedes it.
fn launcher_block(history: &[TaskHistoryEntry]) -> Option<(&TaskHistoryEntry, String, String)> {
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Blocked))?;
    if entry.event != WORKFLOW_RUN_FAILED_EVENT {
        return None;
    }
    let missing = missing_launcher_in(entry.note.as_deref()?)?;
    Some((entry, missing.program, missing.provider))
}

impl OrbitRuntime {
    /// Blocked tasks in this workspace whose block is a missing provider
    /// launcher, each with where that launcher resolves now. Resolution uses
    /// dispatch's own lookup, from this process's `PATH` and `HOME`.
    pub fn infra_blocked_tasks(&self) -> Result<Vec<InfraBlockedTask>, OrbitError> {
        let blocked =
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?;
        let mut infra_blocked = Vec::new();
        for task in blocked {
            if let Some(entry) = self.infra_block_of(&task)? {
                infra_blocked.push(entry);
            }
        }
        Ok(infra_blocked)
    }

    /// Classify one task's current block; `None` unless it is blocked by a
    /// missing provider launcher.
    pub(crate) fn infra_block_of(
        &self,
        task: &Task,
    ) -> Result<Option<InfraBlockedTask>, OrbitError> {
        if task.status != TaskStatus::Blocked {
            return Ok(None);
        }
        let history = self.get_task_history(&task.id)?;
        let Some((entry, program, provider)) = launcher_block(&history) else {
            return Ok(None);
        };
        Ok(Some(InfraBlockedTask {
            task_id: task.id.clone(),
            title: task.title.clone(),
            blocked_at: entry.at,
            run_id: task.job_run_id.clone(),
            launcher: self.locate_provider_launcher(&program),
            program,
            provider,
        }))
    }

    /// Best-effort variant used from run terminalization: a status-write
    /// failure is logged and swallowed so the run still reaches its terminal
    /// state and releases its reservations/file locks.
    ///
    /// `diagnostic` is the `(error_code, message)` a caller knows before its
    /// diagnostic step is durable — orphan reconciliation records that step
    /// only after the terminal write wins. Without it the note falls back to
    /// the run's most recent errored step.
    pub(crate) fn best_effort_block_tasks_for_terminal_run(
        &self,
        run_id: &str,
        state: JobRunState,
        diagnostic: Option<(&str, &str)>,
    ) {
        if let Err(error) = self.block_tasks_for_terminal_run(run_id, state, diagnostic) {
            tracing::warn!(
                run_id,
                state = %state,
                "failed to block tasks coupled to terminal job run: {error}"
            );
        }
    }

    fn block_tasks_for_terminal_run(
        &self,
        run_id: &str,
        state: JobRunState,
        diagnostic: Option<(&str, &str)>,
    ) -> Result<(), OrbitError> {
        let Some(run) = self.get_job_run_backend(run_id)? else {
            return Ok(());
        };
        let (error_code, error_message) = match diagnostic {
            Some((code, message)) => (Some(code.to_string()), Some(message.to_string())),
            None => failed_run_error_context(&run),
        };
        let blocked_update = if state == JobRunState::Interrupted {
            blocked_workflow_interruption_update
        } else {
            blocked_workflow_failure_update
        };
        let tasks = self.list_tasks_filtered(None, None, None, Some(run_id), None, None)?;
        for task in tasks {
            if !task_is_blockable_on_run_failure(task.status) {
                continue;
            }
            let update = blocked_update(
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
                    "failed to block task coupled to terminal job run: {error}"
                );
            }
        }
        Ok(())
    }
}
