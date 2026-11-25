//! The guarded tool families a resident cycle's live grant puts in its tool
//! list. Core still decides every call: the cycle guard checks the live
//! grant, the workspace scope and the recovery gates when the call arrives.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::workflow::ResidentRight;

use super::cycle::live_cycle_from_history;
use super::due::cycle_grant;
use crate::OrbitRuntime;

/// The shipped activity one resident cycle runs.
pub(crate) const RESIDENT_CYCLE_ACTIVITY: &str = "resident_cycle";

const WORKFLOW_TOOL: &str = "orbit.workflow";
const RECOVER_TOOL: &str = "orbit.task.recover";

/// The tool families a right reaches: `dispatch` ships, re-pilots and requeues,
/// `merge` lands, `refresh` refreshes and `recover` resumes a delivery.
/// Promotion uses tools every cycle already has; `plugin_upgrade` reaches no
/// tool, so no cycle installs or upgrades a workspace plugin.
fn tools_of_right(right: ResidentRight) -> &'static [&'static str] {
    match right {
        ResidentRight::Dispatch => &[WORKFLOW_TOOL, RECOVER_TOOL],
        ResidentRight::Merge | ResidentRight::Refresh => &[WORKFLOW_TOOL],
        ResidentRight::Recover => &[RECOVER_TOOL],
        ResidentRight::Promote | ResidentRight::PromoteHeld | ResidentRight::PluginUpgrade => &[],
    }
}

/// The guarded tool families these rights reach, deduplicated, in a stable order.
pub(crate) fn tools_of_rights(rights: &[ResidentRight]) -> Vec<String> {
    let mut tools: Vec<String> = Vec::new();
    for tool in rights.iter().flat_map(|right| tools_of_right(*right)) {
        if !tools.iter().any(|existing| existing == tool) {
            tools.push((*tool).to_string());
        }
    }
    tools.sort();
    tools
}

impl OrbitRuntime {
    /// The guarded tool families the live cycle of each mandate in `task_ids`
    /// holds rights for. A mandate with no live, unexpired cycle, or whose
    /// cycle is observe-only, contributes none.
    pub(crate) fn resident_cycle_right_tools(
        &self,
        task_ids: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        let mut rights = Vec::new();
        for task_id in task_ids {
            let history = self.get_task_history(task_id)?;
            let Some(live) =
                live_cycle_from_history(&history)?.filter(|live| live.expires_at > Utc::now())
            else {
                continue;
            };
            if let Some(grant) = cycle_grant(&history, &live.cycle)
                && !grant.observe_only
            {
                rights.extend(grant.rights);
            }
        }
        Ok(tools_of_rights(&rights))
    }
}
