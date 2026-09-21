//! The `orbit plugin` lifecycle: install, enable, disable, remove, inspect,
//! validate, migrate and sync (design `docs/design/plugins/1_scope.md` §3).
//!
//! Install is global only: a plugin lives once per host under
//! `~/.orbit/plugins/<ns>/<version>/` and every workspace on that host shares
//! it. The repository commits only `.orbit/plugins.yaml`.

mod inspect;
mod install;
mod lifecycle;

#[cfg(test)]
mod tests;

pub use inspect::{
    PluginDoctorResult, PluginSummary, PluginToolSummary, PluginValidationReport, list_plugins,
    plugin_doctor, show_plugin, validate_plugin_dir,
};
pub use install::{PluginAddOptions, install_plugin};
pub use lifecycle::{
    PluginMigrateRequest, PluginSyncOutcome, disable_plugin, enable_plugin,
    migrate_plugin_sidecars, remove_plugin, sync_plugins,
};
