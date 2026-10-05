//! Tool dispatch: audit correlation, agent-identity resolution, the trusted
//! MCP envelope boundary, and the calls a run's plugin broker executes.

mod audit;
#[cfg(unix)]
mod brokered;
mod callback;
mod execute;

pub use audit::{
    AuditContext, audit_role_label, audit_role_label_for_entry_point, trusted_mcp_audit_context,
};
#[cfg(unix)]
pub(crate) use brokered::RunDispatch;
pub(crate) use callback::legacy_callback_identity_enabled;
#[cfg(test)]
pub(crate) use callback::override_activity_tools_for_test;
pub use callback::refuse_plugin_child_cli_command;
pub(super) use execute::execute_global_plugin_dispatch;
pub use execute::{
    ToolDispatchOutcome, ToolEntryPoint, execute_global_in_process_tool_dispatch,
    mark_tool_audit_recorded, take_tool_audit_recorded,
};

/// The environment variable a plugin backend's child carries: the plugin
/// namespace. Informational; identity is the host-issued callback session.
#[cfg(test)]
pub(crate) use orbit_tools::plugin::ORBIT_PLUGIN_ENV;

#[cfg(test)]
mod tests;
