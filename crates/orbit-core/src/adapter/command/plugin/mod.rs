//! Plugin lifecycle as the command surfaces reach it, plus the host-global
//! plugin tool surface the MCP server advertises.

mod host;

pub use crate::app::plugin::{HostPluginMcpCache, HostPluginMcpSurface};
pub use host::{execute_global_plugin_tool, host_plugin_cli_groups, host_plugin_rows};

pub use crate::app::plugin::PluginSkillLink;

pub use crate::app::plugin::{
    PluginAddOptions, PluginEnableOptions, PluginMigrateRequest, PluginPermissionChange,
    PluginRemoveOptions, PluginSecretStatus, PluginSeedOutcome, PluginSummary,
    PluginSummaryJsonProfile, PluginTestOptions, PluginTestReport, PluginTestValidationGate,
    PluginUpgradeOptions, PluginUpgradeResult, plugin_summary_json,
};
pub use crate::runtime::plugin::discovery::{PluginCliGroup, PluginCliVerb};
pub use crate::runtime::plugin::secrets::{MAX_PLUGIN_SECRET_BYTES, PluginSecretValue};
