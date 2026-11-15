//! Experiment's side of the distributed-owned capture port [ORB-22416].

use orbit_common::OrbitError;
use orbit_store::contracts::AdmissionShipContract;
use orbit_types::task::Task;
use orbit_types::workflow::CapturedExperiments;

use crate::OrbitRuntime;
use crate::application::distributed::DistributedExperimentPort;

impl DistributedExperimentPort for OrbitRuntime {
    fn capture_pull_experiments(
        &self,
        task: &Task,
        ship: &AdmissionShipContract,
    ) -> Result<CapturedExperiments, OrbitError> {
        self.capture_claimed_task_experiments(task, ship)
    }
}
