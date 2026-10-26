//! When a task's `granted_tools` may change (design
//! `docs/design/granted-tools/1_scope.md` §1, "Change window").
//!
//! Who may change a grant is decided at the tool chokepoint, which requires
//! operator authority for any `granted_tools` input. This module decides
//! *when*: freely while pre-dispatch, never for a live or preserved attempt.
//! Widening a running agent's authority mid-run would be an unreviewed
//! steering change, so a changed grant waits for a new attempt, which a
//! requeue starts.

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskStatus};

use super::super::lifecycle::live_local_run_owning;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Admit a `granted_tools` change on `task`, or refuse it because the
    /// task has a live or preserved attempt. Call under the task write lock.
    ///
    /// Pre-dispatch means the task is `proposed`, `backlog` or `someday`, no
    /// non-terminal local run's selection names it, and no execution claim
    /// protecting its footprint holds it, and no worktree remains for it.
    /// A stopped attempt's preserved worktree keeps its grant frozen even
    /// after its claim is released. `target` is the status this write leaves
    /// the task in: a write that also starts the task is a dispatch, so it
    /// cannot carry a grant change.
    ///
    /// Returns the run/claim/worktree state the change was made in, for its
    /// history entry.
    pub(super) fn admit_granted_tools_change(
        &self,
        task: &Task,
        target: Option<TaskStatus>,
    ) -> Result<String, OrbitError> {
        let refuse = |reason: String| {
            OrbitError::InvalidInput(format!(
                "task {} cannot change `granted_tools`: {reason}. A grant is frozen once an \
                 attempt is dispatched and takes effect only on a new attempt; requeue the \
                 task and remove any preserved worktree before changing it for the next attempt",
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
        // A newer run binding must not hide an older preserved candidate.
        // Use setup/GC's path derivation, including bundled and fallback trees.
        for run in self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            task_id: Some(task.id.clone()),
            include_steps: false,
            ..Default::default()
        })? {
            for path in orbit_engine::run_worktree_paths(&self.paths().repo_root, &run)? {
                if path.try_exists()? {
                    return Err(refuse(format!(
                        "worktree '{}' still exists for it",
                        path.display()
                    )));
                }
            }
        }
        Ok(match task.job_run_id.as_deref() {
            Some(run_id) => format!(
                "task {}, no live run, protecting claim or worktree (last run `{run_id}` is stopped)",
                task.status
            ),
            None => format!(
                "task {}, no live run, protecting claim or worktree",
                task.status
            ),
        })
    }
}
