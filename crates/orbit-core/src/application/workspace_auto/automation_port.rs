//! Workspace auto's side of the automation-owned board port [ORB-19990].

use orbit_common::OrbitError;
use serde_json::json;

use super::drains::workspace_drains;
use crate::OrbitRuntime;
use crate::application::admission::backlog_exclusion::backlog_snapshot_observed;
use crate::application::automation::AutomationBoardPort;

impl AutomationBoardPort for OrbitRuntime {
    fn eligible_task_ids(&self) -> Result<Vec<String>, OrbitError> {
        let pools = self.auto_crew_pools_for_input(&json!({}))?;
        let snapshot =
            backlog_snapshot_observed(self, "automation_condition", None, &pools, None, None)
                .map_err(|error| {
                    OrbitError::Execution(format!("read eligibility view: {error}"))
                })?;
        Ok(snapshot.admissible_leaves)
    }

    fn local_drain_running(&self) -> Result<bool, OrbitError> {
        Ok(workspace_drains(self)?.0.is_some())
    }
}
