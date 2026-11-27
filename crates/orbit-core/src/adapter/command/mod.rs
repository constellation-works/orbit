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

pub use orbit_tools::tool_suggestions;

#[cfg(unix)]
pub(crate) use dispatch::RunDispatch;
#[cfg(unix)]
pub use dispatch::{
    ClaimedOwnerCall, ClaimedOwnerRoute, bridge_claimed_owner_call, owner_route_unavailable,
};
pub use dispatch::{
    DryRunResult, GlobalAuditStoreCache, GlobalToolCall, ToolCommandCall, ToolEntryPoint,
    audit_role_label, audit_role_label_for_entry_point, execute_global_in_process_tool_dispatch,
    execute_global_in_process_tool_dispatch_with_audit_writer, refuse_plugin_child_cli_command,
    refuse_plugin_child_remote_callback, take_tool_audit_recorded, tool_suggestion_context,
    trusted_mcp_audit_context,
};
pub(crate) use dispatch::{dry_run_audit_role_label, mark_tool_audit_recorded};
pub use plugin::{
    HostPluginMcpCache, HostPluginMcpSurface, MAX_PLUGIN_SECRET_BYTES, PluginAddOptions,
    PluginCliGroup, PluginCliVerb, PluginEnableOptions, PluginMigrateRequest,
    PluginPermissionChange, PluginRemoveOptions, PluginSecretStatus, PluginSecretValue,
    PluginSeedOutcome, PluginSkillLink, PluginSummary, PluginSummaryJsonProfile, PluginTestOptions,
    PluginTestReport, PluginTestValidationGate, PluginUpgradeOptions, PluginUpgradeResult,
    execute_global_plugin_tool, host_plugin_cli_groups, host_plugin_rows, plugin_summary_json,
};
pub use registry::{DoctorResult, DoctorStatus, ToolInfo};
