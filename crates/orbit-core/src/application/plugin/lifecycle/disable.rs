//! `orbit plugin disable`: take the plugin off the host surface.

use orbit_common::OrbitError;

use crate::OrbitRuntime;
use crate::runtime::plugin::paths::plugin_namespace_dir;

use super::super::inspect::PluginSummary;
use super::super::skills::unlink_plugin_skills;
use super::record::{installed_plugin, set_enabled, verified_install_path};

/// Take the plugin off the surface: its tools stop registering, its seeded
/// definitions are skipped with a warning by the clock tick, and its skills
/// are unlinked from provider discovery.
///
/// The seeded files themselves stay: they are workspace content that may
/// carry an operator's edits, and a re-enable must not have to recreate them
/// (§3).
pub fn disable_plugin(runtime: &OrbitRuntime, name: &str) -> Result<PluginSummary, OrbitError> {
    verified_install_path(runtime, &installed_plugin(runtime, name)?)?;
    let summary = set_enabled(runtime, name, false, None, None)?;
    unlink_namespace_skills(runtime, name)?;
    Ok(summary)
}

/// Drop every discovery link that points into this namespace's install family.
///
/// The selector is the namespace directory this host installs into, not the
/// row's `install_path`: a row naming `/home/<user>` would otherwise unlink
/// every skill beneath it, shipped ones included, and a record-only removal
/// has to clean up after a row whose path it deliberately never reads
/// [ORB-12800]. The namespace directory also covers links left pointing at a
/// previously installed version.
pub(super) fn unlink_namespace_skills(
    runtime: &OrbitRuntime,
    name: &str,
) -> Result<(), OrbitError> {
    let global_root = runtime.global_root();
    unlink_plugin_skills(&global_root, &plugin_namespace_dir(&global_root, name))?;
    Ok(())
}
