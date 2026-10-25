//! The executor census's side of the distributed-owned port [ORB-17939].

use orbit_store::contracts::{ExecutorCapability, ExecutorCensus};

use crate::OrbitRuntime;
use crate::application::distributed::DistributedAdmissionPort;

impl DistributedAdmissionPort for OrbitRuntime {
    fn pull_requester_census(
        &self,
        requester: &str,
        capability: ExecutorCapability,
    ) -> Option<ExecutorCensus> {
        self.executor_census(requester, capability)
    }
}
