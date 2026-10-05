//! Per-workspace `[plugin_enablement]` toggles.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_config::{ConfigScope, ConfigStore};
use orbit_tools::plugin::load_plugin_dir;
use orbit_types::plugin::{PluginDisabledLayer, PluginStatus};

use crate::OrbitRuntime;
use crate::runtime::plugin::host::projected_status;

use super::super::inspect::{PluginSummary, summary_for_installed};
use super::enable::{PluginEnableResult, apply_enabled_contributions};
use super::record::{installed_plugin, verified_install_path};

/// Switch a host-enabled plugin back on in this workspace only: write its
/// `[plugin_enablement]` toggle and seed this workspace's routines and
/// auto-tasks.
///
/// A toggle only narrows the host state and never grants anything, so a
/// plugin the host has disabled is refused with a typed error naming the
/// host enable, and nothing is written.
pub fn enable_plugin_in_workspace(
    runtime: &OrbitRuntime,
    name: &str,
    force: bool,
) -> Result<PluginEnableResult, OrbitError> {
    let workspace_config = workspace_config_path(runtime)?;
    let installed = installed_plugin(runtime, name)?;
    if !installed.enabled {
        return Err(OrbitError::PluginDisabledOnHost {
            plugin: name.to_string(),
        });
    }
    let install_path = verified_install_path(runtime, &installed)?;
    // Load before any write, so a manifest that no longer loads fails closed
    // with the toggle untouched.
    let plugin = load_plugin_dir(&install_path)?;
    write_workspace_toggle(&workspace_config, &runtime.global_root(), name, true)?;
    let contributions = apply_enabled_contributions(runtime, &install_path, force)?;
    let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(
        runtime.global_root(),
    ))?;
    let projection = projected_status(&installed, &plugin, &runtime.global_root(), &config.plugins);
    let mut summary = summary_for_installed(
        &installed,
        Some(&plugin),
        projection.status,
        &runtime.global_root(),
    );
    summary.diagnostic = projection.diagnostic;
    summary.workspace_toggle = Some(true);
    Ok(PluginEnableResult {
        summary,
        seeded: contributions.seeded,
        skills: contributions.skills,
        warnings: contributions.warnings,
    })
}

/// Switch a plugin off in this workspace only, by writing `false` to its
/// `[plugin_enablement]` toggle. The host row, other workspaces, skill links
/// (host-level provider discovery) and seeded files are untouched; the clock
/// tick skips this workspace's seeded definitions while the toggle is off.
pub fn disable_plugin_in_workspace(
    runtime: &OrbitRuntime,
    name: &str,
) -> Result<PluginSummary, OrbitError> {
    let workspace_config = workspace_config_path(runtime)?;
    let installed = installed_plugin(runtime, name)?;
    write_workspace_toggle(&workspace_config, &runtime.global_root(), name, false)?;
    let plugin = verified_install_path(runtime, &installed)
        .ok()
        .and_then(|path| load_plugin_dir(&path).ok());
    let mut summary = summary_for_installed(
        &installed,
        plugin.as_ref(),
        PluginStatus::Disabled,
        &runtime.global_root(),
    );
    summary.workspace_toggle = Some(false);
    summary.disabled_by = Some(if installed.enabled {
        PluginDisabledLayer::Workspace
    } else {
        PluginDisabledLayer::Host
    });
    Ok(summary)
}

/// The workspace `config.toml` a toggle is written to. A runtime with no
/// workspace layer (its workspace root is the global root) has no workspace
/// to scope to.
fn workspace_config_path(runtime: &OrbitRuntime) -> Result<PathBuf, OrbitError> {
    if runtime.shared_root() == runtime.global_root() {
        return Err(OrbitError::InvalidInput(
            "`--scope workspace` needs a workspace: run it inside one, or select one with \
             `--workspace`"
                .to_string(),
        ));
    }
    Ok(runtime.shared_root().join("config.toml"))
}

/// Write one `[plugin_enablement]` entry, preserving the rest of the file.
///
/// Creating the file for a toggle is safe: a file holding only the toggle
/// table is not a policy layer, so the replace-only security settings keep
/// inheriting from global.
pub(super) fn write_workspace_toggle(
    path: &Path,
    global_root: &Path,
    name: &str,
    enabled: bool,
) -> Result<(), OrbitError> {
    let mut store = ConfigStore::open(ConfigScope::Workspace, path)?;
    store.set_document_value(
        &orbit_config::plugin_enablement_key(name),
        if enabled { "true" } else { "false" },
    )?;
    store.validate_workspace_with_global(global_root)?;
    store.save()
}

/// This workspace's toggles as they are on disk now, for the long-lived
/// hosts that compare them against a cached runtime's.
pub(crate) fn workspace_plugin_toggles(
    runtime: &OrbitRuntime,
) -> Result<BTreeMap<String, bool>, OrbitError> {
    orbit_config::load_workspace_plugin_enablement(&orbit_config::ConfigRoots::new(
        runtime.global_root(),
        runtime.shared_root(),
    ))
}
