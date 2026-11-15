//! Admission's side of the distributed-owned port [ORB-17939].

use crate::application::task::{TaskListFilter, metadata_task};
use orbit_common::OrbitError;
use orbit_store::contracts::{ExecutorCapability, ExecutorCensus};
use orbit_types::task::TaskStatus;

use crate::OrbitRuntime;
use crate::application::distributed::DistributedAdmissionPort;

impl DistributedAdmissionPort for OrbitRuntime {
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
            .stores()
            .tasks()
            .task_candidates(&open, usize::MAX)?
            .items
            .into_iter()
            .map(|task| (task.id.clone(), metadata_task(task)))
            .collect();
        super::recovery_context::reconcile_recovery_context(self, &mut tasks);
        Ok(())
    }

    fn pull_requester_census(
        &self,
        requester: &str,
        capability: ExecutorCapability,
    ) -> Option<ExecutorCensus> {
        self.executor_census(requester, capability)
    }
}
