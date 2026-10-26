//! When a task's `granted_tools` may change (design
//! `docs/design/granted-tools/1_scope.md` §1, "Change window").
//!
//! Who may change a grant is decided at the tool chokepoint, which requires
//! operator authority for any `granted_tools` input. This module decides
//! *when*: freely while the task is pre-dispatch, never for a live attempt.
//! Widening a running agent's authority mid-run would be an unreviewed
//! steering change, so a changed grant waits for a new attempt, which a
//! requeue starts.

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};

use super::super::lifecycle::live_local_run_owning;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Admit a `granted_tools` change on `task`, or refuse it because the
    /// task has a live attempt. Call under the task write lock.
    ///
    /// Pre-dispatch means the task is `proposed`, `backlog` or `someday`, no
    /// non-terminal local run's selection names it, and no execution claim
    /// protecting its footprint holds it. A worktree a stopped attempt left
    /// behind (a preserved candidate) is no live attempt: whatever resumes it
    /// is a new attempt under the grant it reads then. `target` is the status
    /// this write leaves the task in: a write that also starts the task is a
    /// dispatch, so it cannot carry a grant change.
    ///
    /// Returns the run/claim state the change was made in, for its history
    /// entry.
    pub(super) fn admit_granted_tools_change(
        &self,
        task: &Task,
        target: Option<TaskStatus>,
    ) -> Result<String, OrbitError> {
        let refuse = |reason: String| {
            OrbitError::InvalidInput(format!(
                "task {} cannot change `granted_tools`: {reason}. A grant is frozen once an \
                 attempt is dispatched and takes effect only on a new attempt; requeue the \
                 task, then change it before the next attempt starts",
                task.id
            ))
        };
        if !matches!(
            task.status,
            TaskStatus::Proposed | TaskStatus::Backlog | TaskStatus::Someday
        ) {
            return Err(refuse(format!(
                "it is {}, and a grant changes only while proposed, backlog or someday",
                task.status
            )));
        }
        if target == Some(TaskStatus::InProgress) {
            return Err(refuse(
                "this write starts the task; change the grant on an earlier write".to_string(),
            ));
        }
        if let Some((run_id, state)) = live_local_run_owning(self, &task.id)? {
            return Err(refuse(format!(
                "local run '{run_id}' still owns it ({state})"
            )));
        }
        if let Some(claim) = self
            .inspect_execution_claims()?
            .into_iter()
            .map(|inspection| inspection.claim)
            .find(|claim| claim.task_id == task.id && claim.phase.protects_footprint())
        {
            return Err(refuse(format!(
                "execution claim '{}' holds it",
                claim.claim_id
            )));
        }
        Ok(match task.job_run_id.as_deref() {
            Some(run_id) => format!(
                "task {}, no live run or claim (last run `{run_id}` is stopped)",
                task.status
            ),
            None => format!("task {}, no run or claim", task.status),
        })
    }
}
