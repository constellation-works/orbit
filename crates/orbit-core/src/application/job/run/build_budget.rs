//! Read-only host capacity warnings for running local and pull drains.

use orbit_common::OrbitError;
use orbit_common::process::build_budget::BuildBudgetCapacity;
use orbit_store::contracts::JobRunQuery;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::distributed::PULL_DRAIN_JOB;

/// The admission wrapper a checkout ships to wait for host build slots. A
/// checkout without it never waits, so a slot limit governs none of its builds.
const ADMISSION_WRAPPER: &str = "scripts/build-budget.py";

impl OrbitRuntime {
    /// Compare each running drain's effective worker ceiling with host build
    /// slots, for a checkout that ships the admission wrapper. This observation
    /// never reconciles, resizes, or reserves work.
    pub fn build_budget_capacity_warnings(&self) -> Result<Vec<Value>, OrbitError> {
        // Read settings before the participation check, so an invalid setting
        // is still reported as an advisory error in every checkout.
        let budget = BuildBudgetCapacity::read()?;
        if !self.paths().repo_root.join(ADMISSION_WRAPPER).is_file() {
            return Ok(Vec::new());
        }
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
