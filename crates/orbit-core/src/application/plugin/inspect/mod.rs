//! Read-only plugin surfaces: `list`, `show`, `doctor` and `validate`.

mod doctor;
mod profile;
mod summary;
mod validate;

pub use doctor::{PluginDoctorResult, plugin_doctor};
pub use profile::{PluginRenderedEnvironment, PluginRenderedProfile};
pub(super) use summary::summary_for_installed;
pub use summary::{
    PluginPermissionSummary, PluginSummary, PluginToolSummary, list_plugins, show_plugin,
};
pub use validate::{
    PluginValidationReport, validate_plugin_dir, validate_plugin_dir_for_workspace,
};
