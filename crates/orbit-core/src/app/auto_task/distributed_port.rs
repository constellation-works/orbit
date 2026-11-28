//! Auto-task's side of the distributed-owned auto-task port (ORB-20158).

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry};

use crate::OrbitRuntime;
use crate::application::distributed::DistributedAutoTaskPort;

pub(crate) struct DistributedPort<'a>(pub(crate) &'a OrbitRuntime);

impl DistributedAutoTaskPort for DistributedPort<'_> {
    fn step_bound_claim_hold(
        &self,
        task: &Task,
        history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
    ) -> Option<String> {
        super::job_step::step_bound_admission_hold(self.0, task, history)
    }
}
