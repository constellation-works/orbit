//! Tool command/runtime helpers.
//!
//! Two independent concerns live behind this module and are re-exported here
//! so `command::tool::*` remains the single import path for consumers:
//! - [`dispatch`] — tool dispatch, audit correlation, agent-identity
//!   resolution, and the trusted MCP envelope boundary.
//! - [`registry`] — registry CRUD (list/show/add/remove/enable/disable/doctor).

mod dispatch;
mod plugin;
mod registry;

/// Test-only reach-through to dispatch's activity-allowlist override, so a
/// sibling test module does not import a private dispatch item directly.
#[cfg(test)]
pub(crate) mod dispatch_test_support {
    pub(crate) use super::dispatch::override_activity_tools_for_test;
}

#[cfg(test)]
mod tests;

pub use crate::runtime::tool_exec::DryRunResult;

pub use dispatch::{
    AuditContext, ToolDispatchOutcome, ToolEntryPoint, audit_role_label,
    audit_role_label_for_entry_point, execute_global_in_process_tool_dispatch,
    mark_tool_audit_recorded, take_tool_audit_recorded, trusted_mcp_audit_context,
};
pub use plugin::{
    PluginAddOptions, PluginCliGroup, PluginCliVerb, PluginDoctorResult, PluginEnableOptions,
    PluginEnableResult, PluginLinkSummary, PluginMigrateRequest, PluginPanelSummary,
    PluginPermissionChange, PluginPermissionSummary, PluginSeedAction, PluginSeedOutcome,
    PluginSummary, PluginSyncOutcome, PluginTestOptions, PluginTestOutcome, PluginTestReport,
    PluginToolSummary, PluginUpgradeOptions, PluginUpgradeResult, PluginValidationReport,
    execute_global_plugin_tool, host_plugin_cli_groups, host_plugin_mcp_definitions,
    migrate_plugin_sidecars,
};
pub use registry::{DoctorResult, DoctorStatus, ToolInfo};
