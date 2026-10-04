//! The `orbit plugin` lifecycle: install, enable, disable, remove, inspect,
//! validate, migrate and sync (design `docs/design/plugins/1_scope.md` §3).
//!
//! Install is global only: a plugin lives once per host under
//! `~/.orbit/plugins/<ns>/<version>/` and every workspace on that host shares
//! it. A checkout keeps only its `.orbit/plugins.yaml` pins, which git ignores
//! with the rest of `.orbit/`.
//!
//! Beyond tools, a plugin contributes definitions, seeded schedules
//! ([`seed`]), skills ([`skills`]), a `[plugins.<ns>]` config section,
//! dashboard panels ([`panels`]), conformance goldens ([`conformance`]) and
//! the names of the secrets an operator sets for it ([`secrets`]).
//! Everything a plugin contributes is refused as a unit: a manifest whose
//! definitions break the §4.5 rules registers no tools either, because half a
//! plugin is not a state an operator can reason about.

pub(crate) mod build;
mod conformance;
mod inspect;
mod install;
pub(crate) mod lifecycle;
mod panels;
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
pub use conformance::{PluginTestOptions, PluginTestOutcome, PluginTestReport, test_plugin_dir};
pub use inspect::{
    PluginDoctorResult, PluginPermissionSummary, PluginRenderedEnvironment, PluginRenderedProfile,
    PluginSummary, PluginToolSummary, PluginValidationReport, list_plugins, plugin_build_doctor,
    plugin_doctor, show_plugin, validate_plugin_dir, validate_plugin_dir_for_workspace,
};
pub(crate) use install::install_plugin_reporting_enable;
pub use install::{
    PluginAddOptions, PluginPermissionChange, PluginUpgradeOptions, PluginUpgradeResult,
    install_plugin, upgrade_plugin,
};
pub(crate) use lifecycle::workspace_plugin_toggles;
pub use lifecycle::{
    PluginEnableOptions, PluginEnableResult, PluginMigrateRequest, PluginRemoveOptions,
    PluginSyncOutcome, disable_plugin, disable_plugin_in_workspace, enable_plugin,
    enable_plugin_from_dashboard, enable_plugin_in_workspace, migrate_plugin_sidecars,
    remove_plugin, sync_plugins,
};
pub use panels::{
    PluginLinkSummary, PluginPanelSummary, plugin_panel_refresh_ms, read_plugin_panel,
};
pub use secrets::{
    PluginSecretStatus, list_plugin_secrets, remove_plugin_secret, set_plugin_secret,
};
pub use seed::{PluginSeedAction, PluginSeedOutcome, seed_plugin_definitions};
pub use visibility::{
    InactivePlugin, InactivePluginScope, PluginActivity, inactive_plugin, is_listed,
};
