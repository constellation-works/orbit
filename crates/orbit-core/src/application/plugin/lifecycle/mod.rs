//! Enable, disable, remove, sync and migrate.

mod disable;
mod enable;
mod migrate;
mod record;
mod remove;
mod sync;
mod workspace;

pub use disable::disable_plugin;
pub use enable::{
    PluginEnableOptions, PluginEnableResult, enable_plugin, enable_plugin_from_dashboard,
};
pub(super) use enable::{
    apply_enabled_contributions, resolve_consented_programs, unrequested_grant_warnings,
};
pub use migrate::{PluginMigrateRequest, migrate_plugin_sidecars};
pub(super) use record::{installed_plugin, verified_install_path};
pub use remove::{PluginRemoveOptions, remove_plugin};
pub(super) use sync::build_pin_drift;
pub use sync::{PluginSyncOutcome, sync_plugins};
pub(crate) use workspace::workspace_plugin_toggles;
pub use workspace::{disable_plugin_in_workspace, enable_plugin_in_workspace};
