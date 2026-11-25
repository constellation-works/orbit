//! The guarded tool families a resident cycle's live grant puts in its tool
//! list. Core still decides every call: the cycle guard checks the live
//! grant, the workspace scope and the recovery gates when the call arrives.

use orbit_types::workflow::ResidentRight;

use crate::OrbitRuntime;

/// The shipped activity one resident cycle runs.
const RESIDENT_CYCLE_ACTIVITY: &str = "resident_cycle";

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
fn tools_of_rights(rights: &[ResidentRight]) -> Vec<String> {
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
    /// Discover the families reached by this launch's own live cycle.
    /// The provider's input and its task selectors carry no grant authority.
    pub(crate) fn resident_cycle_granted_tools(&self, run_id: &str, activity: &str) -> Vec<String> {
        if activity != RESIDENT_CYCLE_ACTIVITY {
            return Vec::new();
        }
        self.live_cycle_home_grant_local(run_id)
            .filter(|(_, grant)| !grant.observe_only)
            .map_or_else(Vec::new, |(_, grant)| tools_of_rights(&grant.rights))
    }
}
