//! The workspace auto drain's side of the task-owned port [ORB-17936].

use std::collections::BTreeSet;

use orbit_common::OrbitError;

use super::drains::read_live_leaf_runs;
use crate::OrbitRuntime;
use crate::application::task::TaskWorkspaceAutoPort;

impl TaskWorkspaceAutoPort for OrbitRuntime {
    fn live_leaf_task_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        Ok(read_live_leaf_runs(self)?
            .into_iter()
            .flat_map(|run| run.task_ids)
            .collect())
    }
}
