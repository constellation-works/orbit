use chrono::{Duration, Utc};
use orbit_engine::{WorktreeGcOptions, WorktreeGcResult, collect_worktrees};
use orbit_types::workflow::JobRun;

use crate::{OrbitError, OrbitRuntime};

impl OrbitRuntime {
    /// Delivery jobs own their run worktree until the run is terminal. Reuse
    /// the collector here so delivery and the scheduled GC have identical
    /// task, run, registration, and clean-tree gates.
    pub(crate) fn cleanup_delivered_worktree(
        &self,
        run_id: &str,
    ) -> Result<Option<WorktreeGcResult>, OrbitError> {
        let run = self.show_job_run(run_id)?;
        if !delivery_job_owns_worktree(&run) {
            return Ok(None);
        }

        collect_worktrees(
            &self.paths().repo_root,
            std::slice::from_ref(&run),
            self,
            &WorktreeGcOptions {
                delete: true,
                run_id: Some(run_id.to_string()),
                older_than: None,
            },
        )
        .map(Some)
    }

    pub fn gc_worktrees(
        &self,
        delete: bool,
        run_id: Option<String>,
        older_than_hours: Option<u64>,
    ) -> Result<WorktreeGcResult, OrbitError> {
        let runs = self.list_job_runs(super::job::JobRunListParams::default())?;
        let older_than = older_than_hours
            .map(|hours| {
                let hours = i64::try_from(hours).map_err(|_| {
                    OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                })?;
                Utc::now()
                    .checked_sub_signed(Duration::hours(hours))
                    .ok_or_else(|| {
                        OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                    })
            })
            .transpose()?;
        collect_worktrees(
            &self.paths().repo_root,
            &runs,
            self,
            &WorktreeGcOptions {
                delete,
                run_id,
                older_than,
            },
        )
    }
}

/// These jobs create the task-scoped worktrees that are safe to reap after a
/// successful, completion-authorized delivery. Coordinators such as
/// `workspace_auto_pipeline` and `task_gate_pipeline` only own child runs.
fn delivery_job_owns_worktree(run: &JobRun) -> bool {
    matches!(
        run.job_id.as_str(),
        "task_pr_pipeline" | "task_local_pipeline" | "epic_pipeline"
    )
}
