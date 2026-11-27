//! Read-only plugin surfaces: `list`, `show`, `doctor` and `validate`.

mod doctor;
mod profile;
mod summary;
mod validate;

pub use doctor::{PluginDoctorResult, plugin_build_doctor, plugin_doctor};
pub(super) use summary::summary_for_installed;
pub use summary::{
    PluginSummary, PluginSummaryJsonProfile, list_plugins, plugin_summary_json, show_plugin,
};
pub use validate::{
    PluginValidationReport, validate_plugin_dir, validate_plugin_dir_for_workspace,
};
