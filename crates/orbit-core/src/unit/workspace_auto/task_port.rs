//! The workspace auto drain's side of the task-owned port [ORB-17936].

use std::collections::BTreeSet;

use orbit_common::OrbitError;

use super::drains::read_live_leaf_runs;
use crate::OrbitRuntime;
use crate::application::task::TaskWorkspaceAutoPort;

/// Adapter binding [`TaskWorkspaceAutoPort`] to workspace auto's live leaf
/// reads.
pub(crate) struct TaskPort<'a>(pub(crate) &'a OrbitRuntime);

impl TaskWorkspaceAutoPort for TaskPort<'_> {
    fn live_leaf_task_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        Ok(read_live_leaf_runs(self.0)?
            .into_iter()
            .flat_map(|run| run.task_ids)
            .collect())
    }
}
