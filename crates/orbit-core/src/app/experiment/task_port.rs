//! Experiment's side of the task-owned experiment port [ORB-23015].

use orbit_common::OrbitError;
use orbit_store::contract::ExperimentAssignment;

use crate::OrbitRuntime;
use crate::application::task::TaskExperimentPort;

pub(crate) struct TaskPort<'a>(pub(crate) &'a OrbitRuntime);

impl TaskExperimentPort for TaskPort<'_> {
    fn task_experiment_assignment_readout(
        &self,
        task_id: &str,
    ) -> Result<Vec<ExperimentAssignment>, OrbitError> {
        super::assignment::task_experiments(self.0, task_id)
    }

    fn task_has_experiment_crew_pool(&self, task_id: &str) -> Result<bool, OrbitError> {
        Ok(super::run_policy::experiment_crew_pool(self.0, task_id)?.is_some())
    }
}
