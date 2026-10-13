//! Tool command/runtime helpers.
//!
//! Two independent concerns live behind this module and are re-exported here
//! so `command::tool::*` remains the single import path for consumers:
//! - `dispatch` — tool dispatch, audit correlation, agent-identity
//!   resolution, the trusted MCP envelope boundary, and the calls a run's
//!   plugin broker executes.
//! - `registry` — registry CRUD (list/show/add/remove/enable/disable/doctor).

mod dispatch;
mod plugin;
mod registry;

#[cfg(unix)]
pub(crate) use dispatch::RunDispatch;
pub use dispatch::{
    AuditContext, DryRunResult, ToolDispatchOutcome, ToolEntryPoint, audit_role_label,
    audit_role_label_for_entry_point, execute_global_in_process_tool_dispatch,
    mark_tool_audit_recorded, refuse_plugin_child_cli_command, take_tool_audit_recorded,
    trusted_mcp_audit_context,
};
#[cfg(unix)]
pub use dispatch::{ClaimedOwnerRoute, bridge_claimed_owner_call, owner_route_unavailable};
pub use plugin::{
    HostPluginMcpCache, HostPluginMcpSurface, MAX_PLUGIN_SECRET_BYTES, PluginAddOptions,
    PluginAddResult, PluginCliGroup, PluginCliVerb, PluginDoctorResult, PluginEnableOptions,
    PluginEnableResult, PluginLinkSummary, PluginMigrateRequest, PluginPanelSummary,
    PluginPermissionChange, PluginPermissionSummary, PluginRemoveOptions, PluginSecretStatus,
    PluginSecretValue, PluginSeedAction, PluginSeedOutcome, PluginSkillLink, PluginSummary,
    PluginSyncOutcome, PluginTestOptions, PluginTestOutcome, PluginTestReport, PluginToolSummary,
    PluginUpgradeOptions, PluginUpgradeResult, PluginValidationReport, execute_global_plugin_tool,
    host_plugin_cli_groups, host_plugin_mcp_definitions, host_plugin_rows,
    host_plugin_workspace_tool_owners, migrate_plugin_sidecars,
};
pub use registry::{DoctorResult, DoctorStatus, ToolInfo};
