//! Admission's side of the distributed-owned port [ORB-17939].

use crate::application::task::{TaskListFilter, metadata_task};
use orbit_common::OrbitError;
use orbit_store::contract::{ExecutorCapability, ExecutorCensus};
use orbit_types::task::TaskStatus;

use crate::OrbitRuntime;
use crate::application::distributed::DistributedAdmissionPort;

/// Adapter binding the distributed admission port to the admission unit.
pub(crate) struct DistributedPort<'a>(pub(crate) &'a OrbitRuntime);

impl DistributedAdmissionPort for DistributedPort<'_> {
    fn reconcile_pull_recovery_context(&self) -> Result<(), OrbitError> {
        let open = TaskListFilter {
            statuses: Some(
                TaskStatus::ALL
                    .into_iter()
                    .filter(|status| !status.is_terminal())
                    .collect(),
            ),
            ..Default::default()
        };
        let mut tasks = self
            .0
            .stores()
            .tasks()
            .task_candidates(&open, usize::MAX)?
            .items
            .into_iter()
            .map(|task| (task.id.clone(), metadata_task(task)))
            .collect();
        super::recovery_context::reconcile_recovery_context(self.0, &mut tasks);
        Ok(())
    }

    fn pull_requester_census(
        &self,
        requester: &str,
        capability: ExecutorCapability,
    ) -> Option<ExecutorCensus> {
        super::executor_order::executor_census(self.0, requester, capability)
    }
}
