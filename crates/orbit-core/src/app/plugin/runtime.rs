//! Plugin operations that read the runtime directly: install reporting,
//! host-row invalidation, sandbox visibility and stale callbacks.

use orbit_common::OrbitError;
use orbit_tools::plugin::stale_plugin_callback_session_count as stale_callback_count;

use super::{PluginAddOptions, PluginAddResult};
use crate::OrbitRuntime;
use crate::app::plugin;

pub fn add_plugin(
    rt: &OrbitRuntime,
    source: &str,
    options: &PluginAddOptions,
) -> Result<PluginAddResult, OrbitError> {
    let outcome = plugin::install_plugin_reporting_enable(rt, source, options)?;
    Ok(PluginAddResult {
        summary: outcome.summary,
        seeded: outcome.seeded,
        skills: outcome.skills,
        warnings: outcome.warnings,
    })
}

/// Whether the host plugin rows changed after this runtime built its
/// plugin tool surface, without re-reading the workspace toggles.
///
/// Callers that have already established the workspace config file is
/// unchanged (a config-layer stamp) have established the toggles are
/// too, since they live in that file; for them the row comparison is
/// the complete invalidation check for host plugin rows.
pub(crate) fn plugin_installed_changed(rt: &OrbitRuntime) -> Result<bool, OrbitError> {
    Ok(rt
        .stores()
        .host_store()
        .read()
        .and_then(|snapshot| snapshot.plugin().list())?
        != rt.plugin_load().installed)
}

/// Refuse, naming why, when this process runs inside an agent sandbox
/// that masks plugin state and the plugin secret store.
pub(crate) fn ensure_plugin_state_visible(rt: &OrbitRuntime) -> Result<(), OrbitError> {
    if crate::runtime::plugin::sandbox_mask::plugin_trees_masked(&rt.global_root()) {
        return Err(crate::runtime::plugin::sandbox_mask::not_visible(
            "plugin state and secrets",
        ));
    }
    Ok(())
}

/// Count callback records that no longer name a live plugin backend.
pub(crate) fn stale_plugin_callback_session_count(rt: &OrbitRuntime) -> Result<usize, OrbitError> {
    stale_callback_count(&rt.global_root())
}
