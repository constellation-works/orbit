//! Capability classes an MCP tool call requires and a checkout advertises.
//!
//! Membership in a class follows from what a tool does, so the destination
//! Core can refuse a call the local checkout has no authority to serve
//! [ORB-11012].

use orbit_common::OrbitError;
use orbit_types::tool::mcp_advertised_tool_name;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceCheckoutRole};

/// Capability class of an advertised MCP tool.
///
/// Assigned by what the tool does, so a new tool inherits its class from its
/// behavior rather than from a per-tool registry field [ORB-11012].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolClass {
    /// Discovery and list tools. Never subject to `capability_refused`.
    Unclassified,
    /// Task issuance and the coordination store, whose authority is the
    /// declared control-plane owner rather than the destination host.
    ControlPlane,
    /// Runs, logs, and scheduler state, which the destination host owns.
    Execute,
}

impl McpToolClass {
    pub fn as_str(self) -> &'static str {
        match self {
            McpToolClass::Unclassified => "unclassified",
            McpToolClass::ControlPlane => "control_plane",
            McpToolClass::Execute => "execute",
        }
    }
}

/// Classify one advertised MCP tool by behavior.
///
/// Accepts either the canonical (`orbit.task.add`) or advertised
/// (`orbit_task_add`) spelling. Task reads are `control_plane` because the
/// coordination store is owner-authoritative. A name this host does not
/// advertise is unclassified here; routing rejects it earlier with
/// `tool_not_on_this_host`, which precedes `capability_refused`.
pub fn mcp_tool_class(tool_name: &str) -> McpToolClass {
    match mcp_advertised_tool_name(tool_name).as_str() {
        "orbit_task_add"
        | "orbit_task_update"
        | "orbit_task_start"
        | "orbit_task_approve"
        | "orbit_task_list"
        | "orbit_task_show"
        | "orbit_task_artifact_get"
        | "orbit_task_artifact_put"
        | "orbit_friction_add"
        | "orbit_friction_list"
        | "orbit_friction_update"
        | "orbit_auto_task_list"
        | "orbit_auto_task_mint"
        | "orbit_search"
        | "orbit_workflow_ship" => McpToolClass::ControlPlane,
        // Runs a process on the destination host outside Orbit's sandbox, so
        // the host that would execute it owns the decision — the same reason
        // `orbit.command.exec` is Execute [ORB-11354].
        "orbit_agent_invoke"
        | "orbit_command_exec"
        | "orbit_workflow_run_list"
        | "orbit_workflow_run_show"
        | "orbit_workflow_run_resume"
        | "orbit_workflow_run_workers" => McpToolClass::Execute,
        _ => McpToolClass::Unclassified,
    }
}

/// Capability classes a destination advertises for one workspace checkout.
///
/// Advertisement is a hint that may lag; destination Core refusal stays the
/// correctness boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityClasses {
    control_plane: bool,
    execute: bool,
}

impl CapabilityClasses {
    pub fn new(control_plane: bool, execute: bool) -> Self {
        Self {
            control_plane,
            execute,
        }
    }

    /// Advertisement helper: the classes this local checkout holds.
    ///
    /// An owner checkout is the workspace's control-plane authority and also
    /// runs work locally today. A replica is an execution binding only. A
    /// workspace whose logical record omits `owner_machine_id` predates host
    /// identity and cannot advertise `control_plane` at all. A missing checkout
    /// role is the legacy standalone shape that registry validation
    /// canonicalizes to owner.
    pub fn for_checkout(workspace: &Workspace, checkout: &WorkspaceCheckout) -> Self {
        let is_replica = checkout.role == Some(WorkspaceCheckoutRole::Replica);
        Self {
            control_plane: !is_replica && workspace.owner_machine_id.is_some(),
            execute: true,
        }
    }

    pub fn holds(self, class: McpToolClass) -> bool {
        match class {
            McpToolClass::Unclassified => true,
            McpToolClass::ControlPlane => self.control_plane,
            McpToolClass::Execute => self.execute,
        }
    }
}

/// Destination-side gate: refuse a tool whose class this checkout does not hold.
///
/// Unclassified discovery tools always pass. The refusal carries the class so a
/// caller can distinguish it from a malformed call or an operator capability
/// grant.
pub fn ensure_tool_class_held(tool_name: &str, held: CapabilityClasses) -> Result<(), OrbitError> {
    let class = mcp_tool_class(tool_name);
    if held.holds(class) {
        return Ok(());
    }
    Err(OrbitError::CapabilityRefused(format!(
        "this checkout does not hold the '{}' capability class required by '{tool_name}'",
        class.as_str()
    )))
}
