//! `orbit plugin secret set|list|rm`, and what the rest of the lifecycle does
//! with a plugin's secrets: `enable` names the unset ones, `upgrade` keeps
//! only the still-declared ones, `remove` deletes them and `doctor` reports
//! declared-but-unset ones (design §3, "Plugin secrets").
//!
//! Nothing here returns, formats or logs a value. What leaves this module is
//! a name, whether it is set, and when it was last written.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::{LoadedPlugin, load_plugin_dir};
use orbit_types::plugin::{PluginStatus, is_valid_namespace, is_valid_secret_name};

use crate::OrbitRuntime;
use crate::runtime::plugin::grants::verify_install_path;
use crate::runtime::plugin::secrets::{PluginSecretStore, PluginSecretValue};

use super::inspect::{PluginDoctorResult, PluginSummary};
use super::lifecycle::{installed_plugin, verified_install_path};

/// One secret as `orbit plugin secret list` shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSecretStatus {
    pub name: String,
    pub description: String,
    pub rotatable: bool,
    /// Whether the installed manifest declares it. A stored secret the
    /// manifest no longer names is listed so `rm` can clear it.
    pub declared: bool,
    pub set: bool,
    /// When the value was last written, when it is set.
    pub updated_at: Option<String>,
}

/// The installed plugin whose `spec.secrets` decide what may be set.
fn installed_manifest(runtime: &OrbitRuntime, name: &str) -> Result<LoadedPlugin, OrbitError> {
    let install_path = verified_install_path(runtime, &installed_plugin(runtime, name)?)?;
    Ok(load_plugin_dir(&install_path)?)
}

/// Store `value` as `plugin`'s secret `name`. Only a name the installed
/// manifest declares is accepted.
pub fn set_plugin_secret(
    runtime: &OrbitRuntime,
    plugin: &str,
    name: &str,
    value: &PluginSecretValue,
) -> Result<PluginSecretStatus, OrbitError> {
    let loaded = installed_manifest(runtime, plugin)?;
    let Some(declared) = loaded
        .manifest
        .spec
        .secrets
        .iter()
        .find(|secret| secret.name == name)
    else {
        return Err(undeclared_secret(&loaded, name));
    };
    PluginSecretStore::new(&runtime.global_root()).put(plugin, name, value)?;
    tracing::info!(
        target: "orbit.core.plugin",
        plugin = %plugin,
        secret = %name,
        "plugin secret set",
    );
    let stored = PluginSecretStore::new(&runtime.global_root())
        .list(plugin)?
        .into_iter()
        .find(|entry| entry.name == name);
    Ok(PluginSecretStatus {
        name: name.to_string(),
        description: declared.description.clone(),
        rotatable: declared.rotatable,
        declared: true,
        set: stored.is_some(),
        updated_at: stored.map(|entry| entry.updated_at),
    })
}

fn undeclared_secret(loaded: &LoadedPlugin, name: &str) -> OrbitError {
    let declared: Vec<&str> = loaded
        .manifest
        .spec
        .secrets
        .iter()
        .map(|secret| secret.name.as_str())
        .collect();
    let known = if declared.is_empty() {
        "it declares no secrets".to_string()
    } else {
        format!("it declares {}", declared.join(", "))
    };
    OrbitError::InvalidInput(format!(
        "plugin '{}' does not declare a secret named '{name}' in `spec.secrets`; {known}",
        loaded.namespace()
    ))
}

/// Every declared secret with its set state, then any stored secret the
/// manifest no longer declares.
pub fn list_plugin_secrets(
    runtime: &OrbitRuntime,
    plugin: &str,
) -> Result<Vec<PluginSecretStatus>, OrbitError> {
    let loaded = installed_manifest(runtime, plugin)?;
    let mut stored: BTreeMap<String, String> = PluginSecretStore::new(&runtime.global_root())
        .list(plugin)?
        .into_iter()
        .map(|entry| (entry.name, entry.updated_at))
        .collect();
    let mut rows: Vec<PluginSecretStatus> = loaded
        .manifest
        .spec
        .secrets
        .iter()
        .map(|secret| {
            let updated_at = stored.remove(&secret.name);
            PluginSecretStatus {
                name: secret.name.clone(),
                description: secret.description.clone(),
                rotatable: secret.rotatable,
                declared: true,
                set: updated_at.is_some(),
                updated_at,
            }
        })
        .collect();
    rows.extend(
        stored
            .into_iter()
            .map(|(name, updated_at)| PluginSecretStatus {
                name,
                description: String::new(),
                rotatable: false,
                declared: false,
                set: true,
                updated_at: Some(updated_at),
            }),
    );
    Ok(rows)
}

/// Delete `plugin`'s secret `name`. `Ok(false)` when it was not set.
///
/// Needs no installed manifest: clearing a secret the manifest stopped
/// declaring, or one left by a `--record-only` removal, is exactly when an
/// operator reaches for this.
pub fn remove_plugin_secret(
    runtime: &OrbitRuntime,
    plugin: &str,
    name: &str,
) -> Result<bool, OrbitError> {
    if !is_valid_namespace(plugin) {
        return Err(OrbitError::InvalidInput(format!(
            "'{plugin}' is not a valid plugin namespace"
        )));
    }
    if !is_valid_secret_name(name) {
        return Err(OrbitError::InvalidInput(format!(
            "'{name}' is not a valid secret name"
        )));
    }
    let removed = PluginSecretStore::new(&runtime.global_root()).remove(plugin, name)?;
    if removed {
        tracing::info!(
            target: "orbit.core.plugin",
            plugin = %plugin,
            secret = %name,
            "plugin secret removed",
        );
    }
    Ok(removed)
}

/// Declared secrets `plugin` has no value for, in manifest order.
fn unset_declared_secrets(
    global_root: &Path,
    plugin: &LoadedPlugin,
) -> Result<Vec<String>, OrbitError> {
    if plugin.manifest.spec.secrets.is_empty() {
        return Ok(Vec::new());
    }
    let stored: Vec<String> = PluginSecretStore::new(global_root)
        .list(plugin.namespace())?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    Ok(plugin
        .manifest
        .spec
        .secrets
        .iter()
        .filter(|secret| !stored.contains(&secret.name))
        .map(|secret| secret.name.clone())
        .collect())
}

/// What `enable` (and `add --enable`) tells the operator about secrets the
/// plugin declares but this host has no value for.
pub(super) fn unset_secret_warnings(global_root: &Path, plugin: &LoadedPlugin) -> Vec<String> {
    let namespace = plugin.namespace();
    match unset_declared_secrets(global_root, plugin) {
        Ok(unset) => unset
            .into_iter()
            .map(|name| {
                format!(
                    "secret `{name}` is declared but not set; run `orbit plugin secret set \
                     {namespace} {name}`"
                )
            })
            .collect(),
        Err(error) => vec![format!(
            "could not read the secrets of plugin '{namespace}': {error}"
        )],
    }
}

/// After a new manifest is installed over an old one, drop the secrets it no
/// longer declares. A still-declared secret keeps its value and version.
///
/// Runs after the row names the new tree, so a failure cannot undo the
/// install; it is logged, and the leftover is listed as undeclared by `orbit
/// plugin secret list` for `rm` to clear.
pub(super) fn prune_undeclared_secrets(global_root: &Path, plugin: &LoadedPlugin) {
    let namespace = plugin.namespace();
    match PluginSecretStore::new(global_root)
        .retain(namespace, |name| plugin.manifest.declares_secret(name))
    {
        Ok(removed) if !removed.is_empty() => tracing::info!(
            target: "orbit.core.plugin",
            plugin = %namespace,
            secrets = %removed.join(","),
            "removed secrets the installed manifest no longer declares",
        ),
        Ok(_) => {}
        Err(error) => tracing::warn!(
            target: "orbit.core.plugin",
            plugin = %namespace,
            "could not prune undeclared secrets: {error}",
        ),
    }
}

/// `remove` without `--record-only`: the plugin's secrets go with it.
pub(super) fn delete_plugin_secrets(global_root: &Path, name: &str) -> Result<(), OrbitError> {
    PluginSecretStore::new(global_root).remove_all(name)?;
    Ok(())
}

/// One `doctor` row per declared secret that has no value on this host.
pub(super) fn unset_secret_rows(
    runtime: &OrbitRuntime,
    summaries: &[PluginSummary],
) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    let global_root = runtime.global_root();
    let status: BTreeMap<&str, PluginStatus> = summaries
        .iter()
        .map(|summary| (summary.name.as_str(), summary.status))
        .collect();
    let mut rows = Vec::new();
    for installed in runtime.stores().plugins().list_plugins()? {
        // A row whose install this host refuses is already its own finding;
        // its tree is not evidence of what the plugin declares.
        if verify_install_path(&global_root, &installed).is_err() {
            continue;
        }
        let Ok(plugin) = load_plugin_dir(Path::new(&installed.install_path)) else {
            continue;
        };
        let unset = match unset_declared_secrets(&global_root, &plugin) {
            Ok(unset) => unset,
            Err(error) => {
                rows.push(PluginDoctorResult {
                    plugin: installed.name.clone(),
                    status: PluginStatus::Inactive,
                    message: format!(
                        "could not read the secrets of plugin '{}': {error}",
                        installed.name
                    ),
                });
                continue;
            }
        };
        let plugin_status = status
            .get(installed.name.as_str())
            .copied()
            .unwrap_or(PluginStatus::Disabled);
        for name in unset {
            rows.push(PluginDoctorResult {
                plugin: installed.name.clone(),
                status: plugin_status,
                message: format!(
                    "plugin '{}' declares secret `{name}` but it is not set on this host; run \
                     `orbit plugin secret set {} {name}`",
                    installed.name, installed.name
                ),
            });
        }
    }
    Ok(rows)
}
