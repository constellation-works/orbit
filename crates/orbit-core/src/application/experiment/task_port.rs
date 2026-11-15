//! Experiment's side of the task-owned experiment port [ORB-23015].

use orbit_common::OrbitError;
use orbit_store::contracts::ExperimentAssignment;

use crate::OrbitRuntime;
use crate::application::task::TaskExperimentPort;

impl TaskExperimentPort for OrbitRuntime {
    fn task_experiment_assignment_readout(
        &self,
        task_id: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError> {
        self.task_experiments(task_id)
    }

    fn task_has_experiment_crew_pool(&self, task_id: &str) -> Result<bool, OrbitError> {
        Ok(self.experiment_crew_pool(task_id)?.is_some())
    }
}
