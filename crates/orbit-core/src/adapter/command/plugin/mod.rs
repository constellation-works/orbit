//! Plugin lifecycle as the command surfaces reach it, plus the host-global
//! plugin tool surface the MCP server advertises.

mod host;

pub use crate::application::plugin::{HostPluginMcpCache, HostPluginMcpSurface};
pub use host::{
    execute_global_plugin_tool, host_plugin_cli_groups, host_plugin_mcp_definitions,
    host_plugin_rows,
};

pub use crate::application::plugin::skills::PluginSkillLink;

pub use crate::application::plugin::{
    PluginAddOptions, PluginAddResult, PluginDoctorResult, PluginEnableOptions, PluginEnableResult,
    PluginLinkSummary, PluginMigrateRequest, PluginPanelSummary, PluginPermissionChange,
    PluginPermissionSummary, PluginRemoveOptions, PluginSecretStatus, PluginSeedAction,
    PluginSeedOutcome, PluginSummary, PluginSummaryJsonProfile, PluginSyncOutcome,
    PluginTestOptions, PluginTestOutcome, PluginTestReport, PluginTestValidationGate,
    PluginToolSummary, PluginUpgradeOptions, PluginUpgradeResult, PluginValidationReport,
    migrate_plugin_sidecars, plugin_summary_json,
};
pub use crate::runtime::plugin::discovery::{PluginCliGroup, PluginCliVerb};
pub use crate::runtime::plugin::secrets::{MAX_PLUGIN_SECRET_BYTES, PluginSecretValue};
