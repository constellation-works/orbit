//! The owner clock's revisit of parked auto-task runs (ORB-20158;
//! `docs/design/activity-job/specs/auto-task-step.md` §5).

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::workflow::JobRunState;

use crate::OrbitRuntime;
use crate::application::job::{RunOwnerLiveness, run_owner_liveness};

impl OrbitRuntime {
    /// Hand every due parked run a worker. The run stays the same logical
    /// execution: the worker adopts it under the store's compare-and-set and
    /// continues from its checkpoint, so a repeated wake starts nothing new
    /// and resets no deadline. Owner-only; returns the woken run ids.
    pub(crate) fn wake_parked_job_runs(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Vec<String>, OrbitError> {
        if self.is_write_free() || self.coordination_write_owner().is_some() {
            return Ok(Vec::new());
        }
        let mut woken = Vec::new();
        for run in self.stores().jobs().list_all_pending_or_running_runs()? {
            if run.state != JobRunState::Running
                || run_owner_liveness(&run) == RunOwnerLiveness::Alive
            {
                continue;
            }
            let due = self
                .read_run_state(&run.run_id)?
                .and_then(|state| state.auto_task)
                .is_some_and(|checkpoint| checkpoint.parked_due(now));
            if !due {
                continue;
            }
            // One run's failed spawn must not starve the other due runs.
            match self.spawn_pipeline_worker(&run.run_id, Some("sweep"), false) {
                Ok(()) => woken.push(run.run_id),
                Err(error) => tracing::warn!(
                    target: "orbit.core.sweep",
                    run_id = %run.run_id,
                    error = %error,
                    "could not wake a parked run; retried next tick",
                ),
            }
        }
        Ok(woken)
    }
}
