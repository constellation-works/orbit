//! The installed row every lifecycle verb reads and writes: lookup, install
//! path verification and the host enable flag.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_tools::plugin::load_plugin_dir;
use orbit_types::plugin::{
    InstalledPlugin, PluginDisabledLayer, PluginStatus, parse_stored_grants,
};
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::runtime::plugin::grants::{
    record_authorization, recorded_program_paths, verify_install_path,
};
use crate::runtime::plugin::host::projected_status;
use crate::runtime::plugin::paths::read_pin_file;

use super::super::inspect::{PluginSummary, show_plugin, summary_for_installed};
use super::workspace::workspace_plugin_toggles;

/// The installed tree a lifecycle verb may read from or delete, or the
/// operator-facing refusal that says why it may not.
///
/// The `plugins` row is writable by any backend holding `orbit_tools` (see the
/// `runtime::plugin::grants` module docs), so `install_path` is authority only
/// once it has been held to the install root — the same check the loader
/// applies before it reads the tree [ORB-12785]. Callers run this *before*
/// their first mutation: a refused row is the one an operator most needs to be
/// able to act on, so the refusal must leave the record, and the recovery the
/// diagnostic names, intact [ORB-12800].
pub(in crate::application::plugin) fn verified_install_path(
    runtime: &OrbitRuntime,
    installed: &InstalledPlugin,
) -> Result<PathBuf, OrbitError> {
    verify_install_path(&runtime.global_root(), installed).map_err(OrbitError::PolicyDenied)?;
    Ok(PathBuf::from(&installed.install_path))
}

/// The recorded row for `name`, or the diagnostic naming what to do instead.
pub(in crate::application::plugin) fn installed_plugin(
    runtime: &OrbitRuntime,
    name: &str,
) -> Result<InstalledPlugin, OrbitError> {
    runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| missing_install(runtime, name))
}

/// `programs` is the resolution an enabling command just made; `None` keeps
/// the one last recorded, so a disabled plugin still shows what it was
/// consented to run.
pub(super) fn set_enabled(
    runtime: &OrbitRuntime,
    name: &str,
    enabled: bool,
    grants: Option<&[String]>,
    programs: Option<&BTreeMap<String, PathBuf>>,
) -> Result<PluginSummary, OrbitError> {
    let mut existing = runtime
        .stores()
        .plugins()
        .get_plugin(name)?
        .ok_or_else(|| missing_install(runtime, name))?;
    // `None` (no `--grant` flag, or a disable/remove that never authorizes
    // grants) preserves the recorded set; `Some` — including an explicit
    // empty slice — replaces it completely.
    let grants = match (enabled, grants) {
        (true, Some(grants)) => grants.to_vec(),
        _ => existing.grants.clone(),
    };
    let projection = if enabled {
        let plugin = load_plugin_dir(Path::new(&existing.install_path))?;
        existing.enabled = true;
        existing.grants.clone_from(&grants);
        let config = orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(
            runtime.global_root(),
        ))?;
        Some((
            projected_status(&existing, &plugin, &runtime.global_root(), &config.plugins),
            plugin,
        ))
    } else {
        None
    };
    runtime.with_mutation(|| {
        runtime
            .stores()
            .plugins()
            .set_plugin_enabled(name, enabled, &grants)?;
        let event = if enabled {
            OrbitEvent::PluginEnabled {
                name: name.to_string(),
            }
        } else {
            OrbitEvent::PluginDisabled {
                name: name.to_string(),
            }
        };
        Ok(((), event))
    })?;
    // This command is the authorization, so it is what records the integrity
    // value the loader checks the row back against. Written after the row, so
    // a failure here leaves a plugin that refuses to load and says why, rather
    // than a witness authorizing a grant set the store never took [ORB-12778].
    let programs = programs
        .cloned()
        .unwrap_or_else(|| recorded_program_paths(&runtime.global_root(), name));
    record_authorization(&runtime.global_root(), name, enabled, &grants, &programs)?;
    if let Some((projection, plugin)) = projection {
        let mut summary = summary_for_installed(
            &existing,
            Some(&plugin),
            projection.status,
            &runtime.global_root(),
        );
        summary.diagnostic = projection.diagnostic;
        apply_workspace_toggle(runtime, &mut summary)?;
        return Ok(summary);
    }
    // The live runtime built its registry before this write, so report the
    // stored state rather than the surface this process happens to hold.
    let mut summary = show_plugin(runtime, name)?;
    summary.status = if enabled {
        PluginStatus::Active
    } else {
        PluginStatus::Disabled
    };
    // A row that reaches here was just written from a parsed set, so the
    // spellings parse back; an unreadable one grants nothing, which is what
    // the loader would conclude too.
    let parsed = parse_stored_grants(&grants).unwrap_or_default();
    for permission in &mut summary.permissions {
        permission.granted = parsed.contains(permission.grant);
        permission.granted_roots = parsed
            .entry(permission.grant)
            .and_then(|entry| entry.roots.clone());
    }
    summary.granted = grants;
    summary.diagnostic = None;
    summary.disabled_by = (!enabled).then_some(PluginDisabledLayer::Host);
    apply_workspace_toggle(runtime, &mut summary)?;
    Ok(summary)
}

/// Report a host lifecycle result as this workspace will see it: a host
/// enable does not override a `false` workspace toggle.
fn apply_workspace_toggle(
    runtime: &OrbitRuntime,
    summary: &mut PluginSummary,
) -> Result<(), OrbitError> {
    summary.host_enabled = summary.status != PluginStatus::Disabled;
    summary.workspace_toggle = workspace_plugin_toggles(runtime)?
        .get(&summary.name)
        .copied();
    if summary.host_enabled && summary.workspace_toggle == Some(false) {
        summary.status = PluginStatus::Disabled;
        summary.disabled_by = Some(PluginDisabledLayer::Workspace);
    }
    Ok(())
}

fn missing_install(runtime: &OrbitRuntime, name: &str) -> OrbitError {
    let pinned = read_pin_file(&runtime.shared_root())
        .ok()
        .flatten()
        .is_some_and(|pins| pins.plugins.iter().any(|pin| pin.name == name));
    if pinned {
        OrbitError::InvalidInput(format!(
            "plugin '{name}' is pinned by this workspace but not installed on this host; run \
             `orbit plugin sync` first"
        ))
    } else {
        OrbitError::InvalidInput(format!(
            "plugin '{name}' is not installed on this host; run `orbit plugin add <source>` first"
        ))
    }
}
