use chrono::{Duration, Utc};
use orbit_engine::{WorktreeGcOptions, WorktreeGcResult, collect_worktrees};
use orbit_store::contracts::JobRunQuery;
use orbit_types::workflow::JobRun;

use crate::{OrbitError, OrbitRuntime};

impl OrbitRuntime {
    /// Every recorded run, without step rows. Worktree GC classifies live
    /// worktrees from non-terminal runs and never reads `agent_response_json`.
    pub(crate) fn list_job_runs_for_worktree_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.reconcile_stale_job_runs(None)?;
        self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            include_steps: false,
            ..JobRunQuery::default()
        })
    }

    /// Delivery jobs own their run worktree until the run is terminal. Reuse
    /// the collector here so delivery and the scheduled GC have identical
    /// task, run, registration, and clean-tree gates.
    pub(crate) fn cleanup_delivered_worktree(
        &self,
        run_id: &str,
    ) -> Result<Option<WorktreeGcResult>, OrbitError> {
        let run = self.show_job_run(run_id)?;
        if !self
            .load_v2_job_asset_by_name(&run.job_id)?
            .1
            .owns_task_worktree
        {
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
                estimate_bytes: false,
            },
        )
        .map(Some)
    }

    pub fn gc_worktrees(
        &self,
        delete: bool,
        run_id: Option<String>,
        older_than_hours: Option<u64>,
        estimate_bytes: bool,
    ) -> Result<WorktreeGcResult, OrbitError> {
        let older_than = older_than_hours
            .map(|hours| {
                let hours = i64::try_from(hours).map_err(|_| {
                    OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                })?;
                let duration = Duration::try_hours(hours).ok_or_else(|| {
                    OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                })?;
                Utc::now().checked_sub_signed(duration).ok_or_else(|| {
                    OrbitError::InvalidInput("--older-than-hours is too large".to_string())
                })
            })
            .transpose()?;
        let runs = self.list_job_runs_for_worktree_gc()?;
        collect_worktrees(
            &self.paths().repo_root,
            &runs,
            self,
            &WorktreeGcOptions {
                delete,
                run_id,
                older_than,
                estimate_bytes,
            },
        )
    }
}
