//! Workspace auto's side of the automation-owned board port [ORB-19990].

use orbit_common::OrbitError;
use serde_json::json;

use super::drains::workspace_drains;
use crate::OrbitRuntime;
use crate::application::automation::AutomationBoardPort;
use crate::unit::admission::backlog_snapshot_observed;

/// Adapter binding [`AutomationBoardPort`] to workspace auto's board reads.
pub(crate) struct AutomationPort<'a>(pub(crate) &'a OrbitRuntime);

impl AutomationBoardPort for AutomationPort<'_> {
    fn eligible_task_ids(&self) -> Result<Vec<String>, OrbitError> {
        let pools = self.0.auto_crew_pools_for_input(&json!({}))?;
        let snapshot = backlog_snapshot_observed(
            self.0,
            "automation_condition",
            None,
            &pools,
            None,
            None,
            None,
        )
        .map_err(|error| OrbitError::Execution(format!("read eligibility view: {error}")))?;
        Ok(snapshot.admissible_leaves)
    }

    fn local_drain_running(&self) -> Result<bool, OrbitError> {
        Ok(workspace_drains(self.0)?.0.is_some())
    }
}
