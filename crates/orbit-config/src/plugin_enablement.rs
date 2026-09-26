//! Workspace-scoped plugin enable toggles: the `[plugin_enablement]` table.
//!
//! A workspace `config.toml` may carry `<ns> = true|false` under
//! `[plugin_enablement]`. The toggle narrows the host enable state for that
//! one workspace; it never widens it, so a plugin the host has not enabled
//! stays off whatever the toggle says. An absent entry inherits (on).
//!
//! Two rules make the table safe to write on a checkout that had no
//! `config.toml` before:
//! - the table is global-refused: host enablement lives in the plugin store,
//!   so a global `[plugin_enablement]` would be a second, contradicting
//!   switch and is refused at load;
//! - the table is not a config *policy* layer: a workspace file holding only
//!   this table leaves the replace-only security keys inheriting from global,
//!   exactly as if the file did not exist. Toggling a plugin must never
//!   silently reset the sandbox, approval, or environment allowlist.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::ConfigRoots;

/// The reserved workspace table that holds per-plugin toggles.
pub const PLUGIN_ENABLEMENT_TABLE: &str = "plugin_enablement";

/// Dotted config-store key for one plugin's workspace toggle.
pub fn plugin_enablement_key(namespace: &str) -> String {
    format!("{PLUGIN_ENABLEMENT_TABLE}.{namespace}")
}

/// Read only the workspace layer's toggles, without resolving the rest of the
/// config. Cheap enough for a long-lived host to poll: the dashboard and the
/// MCP server compare it against the toggles a cached runtime was built with.
/// No distinct workspace layer means no toggles.
pub fn load_workspace_plugin_enablement(
    roots: &ConfigRoots,
) -> Result<BTreeMap<String, bool>, OrbitError> {
    if !roots.has_workspace_layer() {
        return Ok(BTreeMap::new());
    }
    let path = roots.workspace().join("config.toml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to read runtime config '{}': {error}",
                redact_home_dir(&path.display().to_string())
            )));
        }
    };
    let document = toml::from_str::<toml::Value>(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {error}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    plugin_enablement_from_document(&document, &path)
}

/// Parse and validate the toggles one document declares.
///
/// Each entry must name a valid plugin namespace and hold a boolean; anything
/// else is refused naming the file, so a typo cannot silently leave a plugin
/// on. Whether the namespace is installed is not known here — the runtime
/// warns about and ignores an unknown one.
pub(crate) fn plugin_enablement_from_document(
    document: &toml::Value,
    path: &Path,
) -> Result<BTreeMap<String, bool>, OrbitError> {
    let Some(value) = document
        .as_table()
        .and_then(|table| table.get(PLUGIN_ENABLEMENT_TABLE))
    else {
        return Ok(BTreeMap::new());
    };
    let display = || redact_home_dir(&path.display().to_string());
    let Some(table) = value.as_table() else {
        return Err(OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': '{PLUGIN_ENABLEMENT_TABLE}' must be a table of \
             `<plugin> = true|false`",
            display()
        )));
    };
    let mut toggles = BTreeMap::new();
    for (namespace, value) in table {
        if !orbit_types::plugin::is_valid_namespace(namespace) {
            return Err(OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': '{PLUGIN_ENABLEMENT_TABLE}.{namespace}' is not a \
                 plugin namespace: use lowercase letters, digits, '_' or '-', starting with a \
                 letter",
                display()
            )));
        }
        let Some(enabled) = value.as_bool() else {
            return Err(OrbitError::InvalidInput(format!(
                "invalid runtime config '{}': '{PLUGIN_ENABLEMENT_TABLE}.{namespace}' must be \
                 true or false",
                display()
            )));
        };
        toggles.insert(namespace.clone(), enabled);
    }
    Ok(toggles)
}

/// Refuse `[plugin_enablement]` in the global `config.toml`.
///
/// Host enablement is the plugin store row `orbit plugin enable|disable`
/// writes; a global toggle table would be a second host switch that the
/// grants witness does not cover.
pub(crate) fn reject_global_plugin_enablement(
    document: &toml::Value,
    path: &Path,
) -> Result<(), OrbitError> {
    let present = document
        .as_table()
        .is_some_and(|table| table.contains_key(PLUGIN_ENABLEMENT_TABLE));
    if !present {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "[{PLUGIN_ENABLEMENT_TABLE}] is a workspace setting: remove it from '{}'. Host \
         enablement is `orbit plugin enable|disable <plugin>`; a per-workspace toggle is \
         `orbit plugin enable|disable <plugin> --scope workspace`",
        redact_home_dir(&path.display().to_string())
    )))
}

/// Remove the toggle table from a workspace document, reporting whether the
/// file is still a policy layer. Only a file that held the table and nothing
/// else stops being one: an existing file without the table — even an empty
/// or comment-only one — keeps counting as a workspace layer, as it always
/// has.
pub(crate) fn strip_plugin_enablement(document: &mut toml::Value) -> bool {
    match document.as_table_mut() {
        Some(table) => table.remove(PLUGIN_ENABLEMENT_TABLE).is_none() || !table.is_empty(),
        None => true,
    }
}

/// Whether a workspace `config.toml` is a config policy layer: it exists and
/// is not a file holding only `[plugin_enablement]`.
///
/// Only such a file stops the replace-only security keys inheriting from
/// global. An unreadable or malformed file counts as a policy layer, so a
/// display that relies on this never understates the exception.
pub fn workspace_config_sets_policy(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return true;
    };
    let Ok(mut document) = toml::from_str::<toml::Value>(&raw) else {
        return true;
    };
    strip_plugin_enablement(&mut document)
}
