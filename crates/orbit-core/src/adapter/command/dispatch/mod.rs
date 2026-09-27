//! Tool dispatch: audit correlation, agent-identity resolution, and the
//! trusted MCP envelope boundary.

mod audit;
mod callback;
mod execute;

pub use audit::{
    AuditContext, audit_role_label, audit_role_label_for_entry_point, trusted_mcp_audit_context,
};
pub(crate) use callback::legacy_callback_identity_enabled;
pub use callback::refuse_plugin_child_cli_command;
#[cfg(test)]
pub(crate) use callback::{
    ActivityToolPolicyEnv, activity_tool_policy_from_env_values,
    override_activity_tool_policy_for_test, override_activity_tools_for_test,
};
pub(super) use execute::execute_global_plugin_dispatch;
pub use execute::{
    ToolDispatchOutcome, ToolEntryPoint, execute_global_in_process_tool_dispatch,
    mark_tool_audit_recorded, take_tool_audit_recorded,
};

#[cfg(test)]
pub(super) use crate::runtime::run_input::ORBIT_MANAGED_RUN_CONTEXT_ENV;
/// The environment variable a plugin backend's child carries: the plugin
/// namespace. Informational; identity is the host-issued callback session.
#[cfg(test)]
pub(crate) use orbit_tools::plugin::ORBIT_PLUGIN_ENV;

#[cfg(test)]
mod tests;
