//! `orbit plugin add`: resolve a source, refuse an in-repository one, copy the
//! tree into the host install root, and record it.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::{
    PluginValidationPolicy, first_party_source, load_plugin_dir, manifest_refusal,
    plugin_symlink_refusal, refuse_plugin_tree_symlinks, resolve_plugin_source,
    validate_loaded_plugin,
};
use orbit_types::plugin::{InstalledPlugin, PluginStatus};
use orbit_types::record::OrbitEvent;

use crate::OrbitRuntime;
use crate::runtime::plugin_host::{plugin_current_link, plugin_install_path, unmet_requirement};

use super::inspect::{PluginSummary, summary_for_installed};

#[derive(Debug, Clone, Default)]
pub struct PluginAddOptions {
    /// Replace an existing install of the same namespace and version.
    pub force: bool,
    /// Enable the plugin as part of the install.
    pub enable: bool,
    /// Grants recorded when `enable` is set.
    pub grants: Vec<String>,
}

/// Install `source` for this host: a local directory, a `git+<url>#<ref>`
/// reference, or a tar archive.
pub fn install_plugin(
    runtime: &OrbitRuntime,
    source: &str,
    options: &PluginAddOptions,
) -> Result<PluginSummary, OrbitError> {
    let resolved = resolve_plugin_source(source)?;
    let source_root = resolved.root.clone();
    refuse_in_repository_source(runtime, &source_root)?;

    let plugin = load_plugin_dir(&source_root)?;
    let first_party =
        plugin.manifest.claims_first_party_namespace() && first_party_source(source, &source_root);
    let policy = PluginValidationPolicy::host_default().with_first_party_verified(first_party);
    validate_loaded_plugin(&plugin, &policy).map_err(manifest_refusal)?;

    let name = plugin.namespace().to_string();
    let version = plugin.manifest.metadata.version.clone();
    let global_root = runtime.global_root();
    let install_path = plugin_install_path(&global_root, &name, &version);
    if install_path.exists() {
        if !options.force {
            return Err(OrbitError::InvalidInput(format!(
                "plugin '{name}' v{version} is already installed at {}; pass --force to replace it",
                install_path.display()
            )));
        }
        std::fs::remove_dir_all(&install_path).map_err(|error| {
            OrbitError::Io(format!("replace {}: {error}", install_path.display()))
        })?;
    }
    copy_tree(&source_root, &install_path)?;
    link_current(&global_root, &name, &version)?;

    let existing = runtime.stores().plugins().get_plugin(&name)?;
    let enabled = options.enable || existing.as_ref().is_some_and(|plugin| plugin.enabled);
    let grants = if options.enable {
        orbit_types::plugin::parse_grants(&options.grants)
            .map_err(OrbitError::InvalidInput)?
            .into_iter()
            .map(|grant| grant.as_str().to_string())
            .collect()
    } else {
        existing.map(|plugin| plugin.grants).unwrap_or_default()
    };
    let record = InstalledPlugin {
        name: name.clone(),
        version: version.clone(),
        source: source.to_string(),
        install_path: install_path.to_string_lossy().into_owned(),
        manifest_digest: plugin.manifest_digest.clone(),
        enabled,
        grants,
        first_party,
        // The store keeps the original `installed_at`; these are the values a
        // fresh row takes.
        installed_at: String::new(),
        updated_at: String::new(),
    };
    runtime.with_mutation(|| {
        runtime.stores().plugins().upsert_plugin(&record)?;
        Ok((
            (),
            OrbitEvent::PluginInstalled {
                name: name.clone(),
                version: version.clone(),
            },
        ))
    })?;

    // `--enable` is an enable: the plugin's schedules are seeded and its
    // skills linked here too, so a one-step install leaves the same state as
    // `add` followed by `enable`. A plugin whose definitions break the §4.5
    // rules contributes nothing and is reported inactive — the refusal belongs
    // to the load, which states it on every later command, so it is not raised
    // as this command's error and the install record stands.
    let contributions_refused = if enabled {
        // `--force` on `add` replaces an install of the same version; it is
        // deliberately not an answer about a definition the operator edited.
        // Overwriting one of those stays `orbit plugin enable <ns> --force`.
        match super::lifecycle::apply_enabled_contributions(runtime, &install_path, false) {
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(
                    target: "orbit.core.plugin",
                    plugin = %name,
                    "installed, but its definitions and skills were not applied: {error}",
                );
                Some(error.to_string())
            }
        }
    } else {
        None
    };

    // Report the status the next runtime will register it with, so `add
    // --enable` on an incompatible host says so now rather than at first use.
    let status = if !enabled {
        PluginStatus::Disabled
    } else if unmet_requirement(&plugin).is_some() || contributions_refused.is_some() {
        PluginStatus::Inactive
    } else {
        PluginStatus::Active
    };
    let stored = runtime
        .stores()
        .plugins()
        .get_plugin(&name)?
        .unwrap_or(record);
    // Held until the copy above finished, so a fetched tree is not collected
    // out from under it.
    drop(resolved);
    let mut summary = summary_for_installed(&stored, Some(&plugin), status);
    summary.diagnostic = contributions_refused;
    Ok(summary)
}

/// Global install only (§3): a plugin tree inside the repository would be
/// vendored state the workspace must not carry.
fn refuse_in_repository_source(
    runtime: &OrbitRuntime,
    source_root: &Path,
) -> Result<(), OrbitError> {
    let repo_root = runtime.paths().repo_root.clone();
    let Ok(repo_root) = std::fs::canonicalize(&repo_root) else {
        return Ok(());
    };
    if !source_root.starts_with(&repo_root) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "refusing to install '{}': it is inside the repository at {}. Orbit plugins are \
         global-install-only — the host installs once under `~/.orbit/plugins/` and the \
         repository commits only `.orbit/plugins.yaml`, so a plugin tree is never vendored \
         into a checkout. Move the plugin outside the repository and add it from there, or \
         pin it in `.orbit/plugins.yaml` and run `orbit plugin sync`.",
        source_root.display(),
        repo_root.display()
    )))
}

fn link_current(global_root: &Path, name: &str, version: &str) -> Result<(), OrbitError> {
    let link = plugin_current_link(global_root, name);
    if link.exists() || link.symlink_metadata().is_ok() {
        std::fs::remove_file(&link)
            .or_else(|_| std::fs::remove_dir_all(&link))
            .map_err(|error| OrbitError::Io(format!("replace {}: {error}", link.display())))?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(Path::new(version), &link)
            .map_err(|error| OrbitError::Io(format!("link {}: {error}", link.display())))?;
    }
    #[cfg(not(unix))]
    {
        // No symlink guarantee off Unix: record the current version as a file
        // beside the install directories instead.
        std::fs::write(&link, version)
            .map_err(|error| OrbitError::Io(format!("write {}: {error}", link.display())))?;
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), OrbitError> {
    refuse_plugin_tree_symlinks(source)?;
    copy_tree_inner(source, source, target)
}

fn copy_tree_inner(tree_root: &Path, source: &Path, target: &Path) -> Result<(), OrbitError> {
    std::fs::create_dir_all(target)
        .map_err(|error| OrbitError::Io(format!("create {}: {error}", target.display())))?;
    for entry in std::fs::read_dir(source)
        .map_err(|error| OrbitError::Io(format!("read {}: {error}", source.display())))?
    {
        let entry =
            entry.map_err(|error| OrbitError::Io(format!("read {}: {error}", source.display())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", path.display())))?;
        if file_type.is_symlink() {
            let target = std::fs::read_link(&path).ok();
            return Err(OrbitError::InvalidInput(plugin_symlink_refusal(
                path.strip_prefix(tree_root).unwrap_or(&path),
                target.as_deref(),
            )));
        }
        let destination = target.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree_inner(tree_root, &path, &destination)?;
        } else {
            std::fs::copy(&path, &destination)
                .map_err(|error| OrbitError::Io(format!("copy {}: {error}", path.display())))?;
            copy_permissions(&path, &destination)?;
        }
    }
    Ok(())
}

fn copy_permissions(source: &Path, target: &Path) -> Result<(), OrbitError> {
    #[cfg(unix)]
    {
        let metadata = std::fs::metadata(source)
            .map_err(|error| OrbitError::Io(format!("stat {}: {error}", source.display())))?;
        std::fs::set_permissions(target, metadata.permissions())
            .map_err(|error| OrbitError::Io(format!("chmod {}: {error}", target.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (source, target);
    }
    Ok(())
}
