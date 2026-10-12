//! Plugin lifecycle as the command surfaces reach it, plus the host-global
//! plugin tool surface the MCP server advertises.

mod host;
mod mcp_surface;
mod runtime;

pub(super) use host::global_only_config;
pub use host::{
    execute_global_plugin_tool, host_plugin_cli_groups, host_plugin_mcp_definitions,
    host_plugin_rows, host_plugin_workspace_tool_owners, migrate_plugin_sidecars,
};
pub use mcp_surface::{HostPluginMcpCache, HostPluginMcpSurface};

pub use crate::application::plugin::skills::PluginSkillLink;

pub use crate::application::plugin::{
    PluginAddOptions, PluginDoctorResult, PluginEnableOptions, PluginEnableResult,
    PluginLinkSummary, PluginMigrateRequest, PluginPanelSummary, PluginPermissionChange,
    PluginPermissionSummary, PluginRemoveOptions, PluginSecretStatus, PluginSeedAction,
    PluginSeedOutcome, PluginSummary, PluginSyncOutcome, PluginTestOptions, PluginTestOutcome,
    PluginTestReport, PluginToolSummary, PluginUpgradeOptions, PluginUpgradeResult,
    PluginValidationReport,
};
pub use crate::runtime::plugin::discovery::{PluginCliGroup, PluginCliVerb};
pub use crate::runtime::plugin::secrets::{MAX_PLUGIN_SECRET_BYTES, PluginSecretValue};

/// What `orbit plugin add --enable` produced, kept alongside the install
/// summary rather than collapsed into it, so the CLI can render seeded
/// schedules, linked skills and warnings the same way `orbit plugin enable`
/// does. `seeded`, `skills` and `warnings` are empty when `--enable` was not
/// set.
#[derive(Debug, Clone)]
pub struct PluginAddResult {
    pub summary: PluginSummary,
    pub seeded: Vec<PluginSeedOutcome>,
    pub skills: Vec<PluginSkillLink>,
    pub warnings: Vec<String>,
}
