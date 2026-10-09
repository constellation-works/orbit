//! The requested-permission diff an upgrade carries or re-asks for.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_tools::plugin::{LoadedPlugin, physical_with_missing_tail};
use orbit_types::plugin::{
    PluginGrant, PluginManifest, PluginNetworkPermission, PluginTemplateVars, render_template,
    template_references,
};

use crate::OrbitRuntime;
use crate::runtime::plugin::config::plugin_config_section;
use crate::runtime::plugin::paths::{plugin_install_path, plugin_state_dir};

use super::PluginPermissionChange;

pub(super) fn permission_diff(
    previous: &LoadedPlugin,
    requested: &LoadedPlugin,
    fs_layout: Option<&FsCompareLayout>,
) -> Result<Vec<PluginPermissionChange>, String> {
    // Render once per manifest. Raw grant strings stay the diff operators
    // already see; resolved roots catch a `{{config.*}}` default that moves
    // the directory those strings name.
    let resolved = match fs_layout {
        Some(layout) => Some((
            resolved_fs_roots(previous, layout)?,
            resolved_fs_roots(requested, layout)?,
        )),
        None => None,
    };
    let mut changes = Vec::new();
    for (before, after) in previous
        .manifest
        .grant_requests()
        .into_iter()
        .zip(requested.manifest.grant_requests())
    {
        let raw_differs = before.requested != after.requested;
        let raw_widened = request_widened(before.grant, &previous.manifest, &requested.manifest);
        let resolved_widened = match (&resolved, before.grant) {
            (Some((previous_roots, requested_roots)), PluginGrant::Fs) => {
                resolved_list_widened(&previous_roots.read, &requested_roots.read)
                    || resolved_list_widened(&previous_roots.write, &requested_roots.write)
            }
            _ => false,
        };
        if !raw_differs && !resolved_widened {
            continue;
        }
        // When the template text did not grow but the directory it opens did,
        // name those directories. The raw strings are identical and would
        // hide the widening from the re-consent message.
        let (previous_text, requested_text) = match &resolved {
            Some((previous_roots, requested_roots)) if resolved_widened && !raw_widened => (
                Some(describe_resolved_fs(previous_roots)),
                Some(describe_resolved_fs(requested_roots)),
            ),
            _ => (before.requested, after.requested),
        };
        changes.push(PluginPermissionChange {
            grant: before.grant,
            previous: previous_text,
            requested: requested_text,
            widened: raw_widened || resolved_widened,
        });
    }
    Ok(changes)
}

/// Host layout both manifests are rendered against.
///
/// `plugin_root` is the path the upgrade will install, used for both sides,
/// so a version-directory rename is not itself a new root. `sections` is the
/// global `[plugins.<ns>]` map only: the recorded `fs` grant is host-wide, and
/// a workspace override must not hide a default another workspace would open.
pub(super) struct FsCompareLayout {
    workspace: String,
    plugin_root: PathBuf,
    plugin_state: PathBuf,
    sections: BTreeMap<String, serde_json::Value>,
}

pub(super) fn fs_compare_layout(
    runtime: &OrbitRuntime,
    global_root: &Path,
    name: &str,
    version: &str,
) -> Result<FsCompareLayout, OrbitError> {
    let config =
        orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(global_root))?;
    let repo_root = &runtime.paths().repo_root;
    let workspace = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.clone());
    Ok(FsCompareLayout {
        workspace: workspace.to_string_lossy().into_owned(),
        plugin_root: plugin_install_path(global_root, name, version),
        plugin_state: plugin_state_dir(global_root, name),
        sections: config.plugins,
    })
}

pub(super) fn fs_request_references_config(manifest: &PluginManifest) -> bool {
    manifest
        .spec
        .permissions
        .fs
        .read
        .iter()
        .chain(manifest.spec.permissions.fs.write.iter())
        .any(|entry| {
            template_references(entry)
                .iter()
                .any(|reference| reference.starts_with("config."))
        })
}

struct ResolvedFsRoots {
    read: Vec<PathBuf>,
    write: Vec<PathBuf>,
}

/// Render one manifest's fs lists the way [`orbit_tools::plugin::render_fs_roots`]
/// does (relative roots join the plugin root, never the process cwd), then
/// resolve each root with [`physical_with_missing_tail`], the same answer the
/// sandbox compiles.
fn resolved_fs_roots(
    plugin: &LoadedPlugin,
    layout: &FsCompareLayout,
) -> Result<ResolvedFsRoots, String> {
    let vars = PluginTemplateVars {
        workspace: Some(layout.workspace.clone()),
        plugin_root: layout.plugin_root.to_string_lossy().into_owned(),
        plugin_state: layout.plugin_state.to_string_lossy().into_owned(),
        config: plugin_config_section(plugin, &layout.sections).rendered_values(),
    };
    let permissions = &plugin.manifest.spec.permissions.fs;
    Ok(ResolvedFsRoots {
        read: resolve_root_list(
            &permissions.read,
            &layout.plugin_root,
            &vars,
            "spec.permissions.fs.read",
        )?,
        write: resolve_root_list(
            &permissions.write,
            &layout.plugin_root,
            &vars,
            "spec.permissions.fs.write",
        )?,
    })
}

fn resolve_root_list(
    declared: &[String],
    plugin_root: &Path,
    vars: &PluginTemplateVars,
    field: &str,
) -> Result<Vec<PathBuf>, String> {
    declared
        .iter()
        .enumerate()
        .map(|(index, declared)| {
            let rendered = render_template(declared, vars, &format!("{field}[{index}]"))
                .map_err(|error| error.to_string())?;
            let path = PathBuf::from(rendered);
            let absolute = if path.is_absolute() {
                path
            } else {
                plugin_root.join(path)
            };
            Ok(physical_with_missing_tail(&absolute))
        })
        .collect()
}

/// A requested root widens when no previously opened root already contains it.
/// A child of an opened root is narrower and keeps the carried grant; a
/// parent, a sibling, or any other path does not.
fn resolved_list_widened(previous: &[PathBuf], requested: &[PathBuf]) -> bool {
    requested.iter().any(|requested| {
        !previous
            .iter()
            .any(|previous| requested == previous || requested.starts_with(previous))
    })
}

fn describe_resolved_fs(roots: &ResolvedFsRoots) -> String {
    let mut parts = Vec::new();
    if !roots.read.is_empty() {
        parts.push(format!("read={}", display_roots(&roots.read)));
    }
    if !roots.write.is_empty() {
        parts.push(format!("write={}", display_roots(&roots.write)));
    }
    parts.join(" ")
}

fn display_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn request_widened(
    grant: PluginGrant,
    previous: &PluginManifest,
    requested: &PluginManifest,
) -> bool {
    let before = &previous.spec.permissions;
    let after = &requested.spec.permissions;
    match grant {
        PluginGrant::Fs => {
            contains_added(&before.fs.read, &after.fs.read)
                || contains_added(&before.fs.write, &after.fs.write)
        }
        PluginGrant::Network => network_rank(after.network) > network_rank(before.network),
        PluginGrant::EnvPass => contains_added(&before.env_pass, &after.env_pass),
        PluginGrant::OrbitTools => contains_added(&before.orbit_tools, &after.orbit_tools),
        PluginGrant::Unsandboxed => {
            previous.spec.backend.sandbox != requested.spec.backend.sandbox
                && requested.spec.backend.sandbox == orbit_types::plugin::PluginSandbox::None
        }
    }
}

fn contains_added(previous: &[String], requested: &[String]) -> bool {
    let previous: BTreeSet<&str> = previous.iter().map(String::as_str).collect();
    requested
        .iter()
        .map(String::as_str)
        .any(|value| !previous.contains(value))
}

fn network_rank(permission: PluginNetworkPermission) -> u8 {
    match permission {
        PluginNetworkPermission::None => 0,
        PluginNetworkPermission::Loopback => 1,
        PluginNetworkPermission::Any => 2,
    }
}

pub(super) fn permission_widening_message(
    name: &str,
    manifest: &PluginManifest,
    changes: &[PluginPermissionChange],
    previous_manifest_error: Option<&str>,
) -> String {
    let mut message = String::from(
        "Requested permissions widened; the plugin was disabled and its grants were cleared:\n",
    );
    if let Some(error) = previous_manifest_error {
        message.push_str(&format!(
            "  previous manifest could not be compared safely: {error}\n"
        ));
        for request in manifest
            .grant_requests()
            .into_iter()
            .filter(|request| request.requested.is_some())
        {
            message.push_str(&format!(
                "  {}: (unavailable) -> {}\n",
                request.grant,
                request.requested.as_deref().unwrap_or("(not requested)")
            ));
        }
    } else {
        for change in changes.iter().filter(|change| change.widened) {
            message.push_str(&format!(
                "  {}: {} -> {}\n",
                change.grant,
                change.previous.as_deref().unwrap_or("(not requested)"),
                change.requested.as_deref().unwrap_or("(not requested)")
            ));
        }
    }
    let grants = manifest
        .required_grants()
        .into_iter()
        .map(|grant| grant.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let enable_command = if grants.is_empty() {
        format!("orbit plugin enable {name}")
    } else {
        format!("orbit plugin enable {name} --grant {grants}")
    };
    message.push_str(&format!(
        "Review the new requests, then re-consent with `{enable_command}`."
    ));
    message
}
