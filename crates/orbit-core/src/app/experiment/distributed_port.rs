//! Experiment's side of the distributed-owned capture port [ORB-22416].

use orbit_common::OrbitError;
use orbit_store::contract::AdmissionShipContract;
use orbit_types::task::Task;
use orbit_types::workflow::CapturedExperiments;

use crate::OrbitRuntime;
use crate::application::distributed::DistributedExperimentPort;

pub(crate) struct DistributedPort<'a>(pub(crate) &'a OrbitRuntime);

impl DistributedExperimentPort for DistributedPort<'_> {
    fn capture_pull_experiments(
        &self,
        task: &Task,
        ship: &AdmissionShipContract,
    ) -> Result<CapturedExperiments, OrbitError> {
        super::overlay::capture_claimed_task_experiments(self.0, task, ship)
    }
}
