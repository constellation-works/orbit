//! Read-only host capacity warnings for running local and pull drains.

use orbit_common::OrbitError;
use orbit_common::process::build_budget::BuildBudgetCapacity;
use orbit_store::contracts::JobRunQuery;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::distributed::PULL_DRAIN_JOB;

impl OrbitRuntime {
    /// Compare each running drain's effective worker ceiling with host build
    /// slots. This observation never reconciles, resizes, or reserves work.
    pub fn build_budget_capacity_warnings(&self) -> Result<Vec<Value>, OrbitError> {
        let budget = BuildBudgetCapacity::read()?;
        let mut warnings = Vec::new();
        for job_id in ["workspace_auto_pipeline", PULL_DRAIN_JOB] {
            for run in self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
                job_id: Some(job_id.into()),
                active_only: true,
                include_steps: false,
                ..Default::default()
            })? {
                if run.state == orbit_types::workflow::JobRunState::Pending {
                    continue;
                }
                let submitted = self.submitted_max_active_leaf_runs(&run)?;
                let concurrency = self
                    .read_run_state(&run.run_id)?
                    .map_or(submitted, |state| {
                        state.effective_max_active_leaf_runs(submitted)
                    });
                if let Some(message) = budget.warning(u64::from(concurrency)) {
                    warnings.push(json!({
                        "run_id": run.run_id, "concurrency": concurrency,
                        "build_slots": budget.slots, "settings_file": budget.settings_file,
                        "message": message,
                    }));
                }
            }
        }
        Ok(warnings)
    }
}
