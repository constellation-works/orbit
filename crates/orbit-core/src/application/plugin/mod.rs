//! The `orbit plugin` lifecycle: install, enable, disable, remove, inspect,
//! validate, migrate and sync (design `docs/design/plugins/1_scope.md` §3).
//!
//! Install is global only: a plugin lives once per host under
//! `~/.orbit/plugins/<ns>/<version>/` and every workspace on that host shares
//! it. A checkout keeps only its `.orbit/plugins.yaml` pins, which git ignores
//! with the rest of `.orbit/`.
//!
//! Beyond tools, a plugin contributes definitions, seeded schedules
//! (`seed`), skills (`skills`), a `[plugins.<ns>]` config section,
//! dashboard panels (`panels`), conformance goldens (`conformance`) and
//! the names of the secrets an operator sets for it (`secrets`).
//! Everything a plugin contributes is refused as a unit: a manifest whose
//! definitions break the §4.5 rules registers no tools either, because half a
//! plugin is not a state an operator can reason about.

pub(crate) mod build;
mod conformance;
mod inspect;
mod install;
pub(crate) mod lifecycle;
pub(crate) mod mcp_surface;
mod panels;
mod runtime;
mod secrets;
pub(crate) mod seed;
pub(crate) mod skills;
mod visibility;

#[cfg(test)]
mod tests;

pub(crate) use crate::runtime::plugin::definitions::shipped_job_names;
/// The definition rules and the provenance header live in the runtime kernel:
/// the plugin host applies them while it builds the tool surface, and the
/// lifecycle here reads the same rules when it seeds. Re-exported so one
/// import path serves the whole use case.
pub use crate::runtime::plugin::definitions::{
    PluginDefinition, PluginDefinitionSet, load_plugin_definitions, read_definition_provenance,
    seeded_definition_name,
};
pub use conformance::{
    PluginTestDeferral, PluginTestOptions, PluginTestOutcome, PluginTestReport,
    PluginTestValidationGate, test_plugin_dir,
};
pub use inspect::{
    PluginDoctorResult, PluginPermissionSummary, PluginRenderedEnvironment, PluginRenderedProfile,
    PluginSummary, PluginSummaryJsonProfile, PluginToolSummary, PluginValidationReport,
    list_plugins, plugin_build_doctor, plugin_doctor, plugin_summary_json, show_plugin,
    validate_plugin_dir, validate_plugin_dir_for_workspace,
};
pub(crate) use install::install_plugin_reporting_enable;
pub use install::{
    PluginAddOptions, PluginPermissionChange, PluginUpgradeOptions, PluginUpgradeResult,
    install_plugin, upgrade_plugin,
};
pub use lifecycle::{
    PluginEnableOptions, PluginEnableResult, PluginMigrateRequest, PluginRemoveOptions,
    PluginSyncOutcome, SidecarManifest, disable_plugin, disable_plugin_in_workspace, enable_plugin,
    enable_plugin_from_dashboard, enable_plugin_in_workspace, load_sidecar_manifest,
    migrate_plugin_sidecars, remove_plugin, sync_plugins,
};
pub use mcp_surface::{HostPluginMcpCache, HostPluginMcpSurface};
pub(crate) use mcp_surface::{global_only_config, global_only_config_with_env};
pub use panels::{
    PluginLinkSummary, PluginPanelSummary, plugin_panel_refresh_ms, read_plugin_panel,
};
pub use secrets::{
    PluginSecretStatus, list_plugin_secrets, remove_plugin_secret, set_plugin_secret,
};
pub use seed::{PluginSeedAction, PluginSeedOutcome, seed_plugin_definitions};
use skills::PluginSkillLink;
pub use visibility::{
    InactivePlugin, InactivePluginScope, PluginActivity, inactive_plugin, is_listed,
};

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
