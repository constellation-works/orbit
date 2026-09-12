//! Layered resolution and source provenance.
//!
//! Reads the global and workspace documents, merges them per key into one
//! document that [`crate::resolved`] admits, and answers "where did this value
//! come from?" for `orbit config show`/`get`.
//!
//! Three layering rules live here and nowhere else:
//! - nested tables merge recursively, so a workspace can override one crew
//!   field without restating the crew;
//! - a registry key is one setting, so a workspace value for a registered
//!   table key replaces the global table rather than merging into it;
//! - the replace-only keys below never inherit from global once a distinct
//!   workspace file exists;
//! - an explicit workspace `operation.preset` resets the preset-managed
//!   `operation.*` keys, so a global explicit value for one of them is not
//!   inherited past a workspace preset selection [ORB-11332]. The typed
//!   resolution in [`crate::operation`] is the authority; the merged document
//!   mirrors it so `orbit config show` and the effective policy agree.

use std::fs;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::ConfigRoots;
use crate::operation::{
    OperationLayer, OperationLayerSource, OperationPolicy, OperationPreset, PRESET_MANAGED_KEYS,
};
use crate::persistence::PersistenceConfig;
use crate::registry::CONFIG_KEY_REGISTRY;
use crate::resolved::{ResolvedConfig, warn_compatibility_keys};

/// Security-sensitive settings that a workspace file must restate to keep.
/// Inheriting a machine-global sandbox, approval, or environment allowlist
/// into a workspace that never asked for it is the failure mode this prevents.
const WORKSPACE_REPLACE_ONLY_KEYS: &[&str] = &[
    "execution.codex.approval_policy",
    "execution.codex.sandbox",
    "execution.env.pass",
];

/// Which layer supplied a resolved value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigValueSourceKind {
    /// A compiled-in default.
    BuiltIn,
    /// An environment variable.
    Environment,
    /// The global `config.toml`.
    Global,
    /// The workspace `config.toml`.
    Workspace,
}

impl ConfigValueSourceKind {
    /// Stable label used in `orbit config show` output.
    pub fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "built-in",
            Self::Environment => "environment",
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }
}

/// Where one resolved value came from, including the file when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigValueSource {
    kind: ConfigValueSourceKind,
    path: Option<PathBuf>,
}

impl ConfigValueSource {
    /// The layer that supplied the value.
    pub fn kind(&self) -> ConfigValueSourceKind {
        self.kind
    }

    /// The file that supplied the value, for file-backed layers.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

/// One resolved key with its value and provenance.
#[derive(Debug, Clone)]
pub struct EffectiveConfigValue {
    /// Dotted config key.
    pub key: String,
    /// Resolved value, projected as JSON.
    pub value: serde_json::Value,
    /// Layer the value came from.
    pub source: ConfigValueSource,
}

/// Every resolved key with provenance, for `orbit config show`/`get`.
#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    snapshot: crate::registry::ConfigSnapshot,
    values: Vec<EffectiveConfigValue>,
}

impl EffectiveConfig {
    /// Resolved value for one admitted key, including projected crew fields.
    ///
    /// Configured crew effort is present only when the assignment set it.
    /// A live `crews.<name>.effort` key whose crew exists but omitted the
    /// field returns JSON null rather than inventing a provider default.
    pub fn value_for(&self, key: &str) -> Option<serde_json::Value> {
        if let Some(value) = self.snapshot.value_for(key) {
            return Some(value);
        }
        if let Some(entry) = self.values.iter().find(|entry| entry.key == key) {
            return Some(entry.value.clone());
        }
        if let Ok(Some(parsed)) = crate::registry::parse_crew_field_key(key) {
            let prefix = format!("crews.{}.", parsed.name);
            if self
                .values
                .iter()
                .any(|entry| entry.key.starts_with(&prefix))
            {
                return Some(serde_json::Value::Null);
            }
        }
        None
    }

    /// Every resolved value, sorted by key.
    pub fn values(&self) -> &[EffectiveConfigValue] {
        &self.values
    }
}

/// Load the layered config and attribute every value to the layer it came from.
pub fn load_effective_config(roots: &ConfigRoots) -> Result<EffectiveConfig, OrbitError> {
    let loaded = load_layered_resolved(roots)?;
    let values = effective_values(
        &loaded.resolved,
        loaded.global.as_ref(),
        loaded.workspace.as_ref(),
    );
    Ok(EffectiveConfig {
        snapshot: loaded.resolved.snapshot,
        values,
    })
}

struct ConfigDocument {
    path: PathBuf,
    value: toml::Value,
}

pub(crate) struct LoadedResolvedConfig {
    pub(crate) resolved: ResolvedConfig,
    global: Option<ConfigDocument>,
    workspace: Option<ConfigDocument>,
}

pub(crate) fn load_layered_resolved(
    roots: &ConfigRoots,
) -> Result<LoadedResolvedConfig, OrbitError> {
    let global = read_config_document(&roots.global().join("config.toml"))?;
    let workspace = if roots.has_workspace_layer() {
        read_config_document(&roots.workspace().join("config.toml"))?
    } else {
        None
    };
    let persistence = PersistenceConfig::default_for_roots(roots.global(), roots.workspace());

    if global.is_none() && workspace.is_none() {
        return Ok(LoadedResolvedConfig {
            resolved: ResolvedConfig::built_in(persistence),
            global,
            workspace,
        });
    }

    let mut merged = global
        .as_ref()
        .map(|document| document.value.clone())
        .unwrap_or_else(empty_document);
    if let Some(workspace_document) = &workspace {
        merge_tables(&mut merged, &workspace_document.value);

        // Registry table values are one config key, so a workspace value
        // replaces the global table rather than merging its members.
        // Dynamically named crews are intentionally excluded:
        // their fields layer recursively so one crew field can be overridden.
        for descriptor in CONFIG_KEY_REGISTRY {
            if let Some(value) = value_at_path(&workspace_document.value, descriptor.key) {
                set_value_at_path(&mut merged, descriptor.key, value.clone());
            }
        }
        for key in WORKSPACE_REPLACE_ONLY_KEYS {
            if value_at_path(&workspace_document.value, key).is_none() {
                remove_value_at_path(&mut merged, key);
            }
        }
        if value_at_path(&workspace_document.value, OperationPreset::KEY).is_some() {
            for key in PRESET_MANAGED_KEYS {
                if value_at_path(&workspace_document.value, key).is_none() {
                    remove_value_at_path(&mut merged, key);
                }
            }
        }
    }

    let config_path = workspace
        .as_ref()
        .or(global.as_ref())
        .map(|document| document.path.as_path())
        .unwrap_or_else(|| Path::new("<built-in defaults>"));
    let merged_raw = toml::to_string(&merged).map_err(|err| {
        OrbitError::InvalidInput(format!(
            "failed to render layered runtime config '{}': {err}",
            redact_home_dir(&config_path.display().to_string())
        ))
    })?;
    let mut resolved = ResolvedConfig::from_layered_raw_str(&merged_raw, config_path, persistence)?;
    for document in [global.as_ref(), workspace.as_ref()].into_iter().flatten() {
        warn_compatibility_keys(&document.value, &document.path);
    }
    resolved.operation = resolve_operation_layers(global.as_ref(), workspace.as_ref())?;
    Ok(LoadedResolvedConfig {
        resolved,
        global,
        workspace,
    })
}

/// Resolve operation-mode preferences from the exact layers rather than the
/// merged document, so the preset-reset rule is applied per layer.
fn resolve_operation_layers(
    global: Option<&ConfigDocument>,
    workspace: Option<&ConfigDocument>,
) -> Result<OperationPolicy, OrbitError> {
    let global_layer = global
        .map(|document| OperationLayer::from_document(&document.value, &document.path))
        .transpose()?
        .unwrap_or_default();
    let workspace_layer = workspace
        .map(|document| OperationLayer::from_document(&document.value, &document.path))
        .transpose()?
        .unwrap_or_default();
    Ok(OperationPolicy::resolve(&[
        (OperationLayerSource::Global, &global_layer),
        (OperationLayerSource::Workspace, &workspace_layer),
    ]))
}

fn read_config_document(path: &Path) -> Result<Option<ConfigDocument>, OrbitError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|err| {
        OrbitError::Io(format!(
            "failed to read runtime config '{}': {err}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    let value = toml::from_str(&raw).map_err(|err| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {err}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    Ok(Some(ConfigDocument {
        path: path.to_path_buf(),
        value,
    }))
}

fn empty_document() -> toml::Value {
    toml::Value::Table(toml::map::Map::new())
}

fn merge_tables(base: &mut toml::Value, overlay: &toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base), toml::Value::Table(overlay)) => {
            for (key, value) in overlay {
                match base.get_mut(key) {
                    Some(existing) => merge_tables(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, overlay) => *base = overlay.clone(),
    }
}

pub(crate) fn value_at_path<'a>(document: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    let mut value = document;
    for segment in key.split('.') {
        value = value.as_table()?.get(segment)?;
    }
    Some(value)
}

fn crew_entry<'a>(
    document: &'a toml::Value,
    name: &str,
) -> Option<&'a toml::map::Map<String, toml::Value>> {
    document
        .as_table()?
        .get("crews")?
        .as_table()?
        .get(name)?
        .as_table()
}

fn set_value_at_path(document: &mut toml::Value, key: &str, value: toml::Value) {
    let segments = key.split('.').collect::<Vec<_>>();
    let Some((last, ancestors)) = segments.split_last() else {
        return;
    };
    let mut table = document.as_table_mut();
    for segment in ancestors {
        let Some(current) = table else {
            return;
        };
        let entry = current
            .entry((*segment).to_string())
            .or_insert_with(empty_document);
        table = entry.as_table_mut();
    }
    if let Some(table) = table {
        table.insert((*last).to_string(), value);
    }
}

fn remove_value_at_path(document: &mut toml::Value, key: &str) {
    let segments = key.split('.').collect::<Vec<_>>();
    let Some((last, ancestors)) = segments.split_last() else {
        return;
    };
    let mut value = document;
    for segment in ancestors {
        let Some(next) = value
            .as_table_mut()
            .and_then(|table| table.get_mut(*segment))
        else {
            return;
        };
        value = next;
    }
    if let Some(table) = value.as_table_mut() {
        table.remove(*last);
    }
}

fn effective_values(
    resolved: &ResolvedConfig,
    global: Option<&ConfigDocument>,
    workspace: Option<&ConfigDocument>,
) -> Vec<EffectiveConfigValue> {
    let mut values = resolved
        .snapshot
        .all_values()
        .into_iter()
        .map(|(key, value)| EffectiveConfigValue {
            key: key.to_string(),
            value,
            source: source_for_key(key, global, workspace),
        })
        .collect::<Vec<_>>();

    // `execution.env.inherit` is a derived invariant, not an admitted config
    // key (see `resolved::ExecutionEnvPolicy`), so it does not belong in the
    // `settings`-shaped values here: `config get` rejects it via
    // `admit_config_key`, and a settings-only listing must stay readable by
    // `config get`/`config set`. `orbit config show`'s JSON/text rendering
    // surfaces it separately as a derived field.

    for (name, crew) in &resolved.crews {
        let mut fields = vec![
            ("model", serde_json::json!(crew.assignment.model)),
            ("provider", serde_json::json!(crew.assignment.provider)),
            ("description", serde_json::json!(crew.description)),
            ("tags", serde_json::json!(crew.tags)),
        ];
        // Configured effort only. An omitted field keeps the provider default
        // and must not appear as a fabricated effective setting.
        if let Some(effort) = crew.assignment.effort {
            fields.push(("effort", serde_json::json!(effort)));
        }
        for (field, value) in fields {
            let key = format!("crews.{name}.{field}");
            values.push(EffectiveConfigValue {
                source: source_for_crew_field(name, field, global, workspace),
                key,
                value,
            });
        }
    }
    values.sort_by(|left, right| left.key.cmp(&right.key));
    values
}

fn source_for_key(
    key: &str,
    global: Option<&ConfigDocument>,
    workspace: Option<&ConfigDocument>,
) -> ConfigValueSource {
    if let Some(document) = workspace
        && value_at_path(&document.value, key).is_some()
    {
        return file_source(ConfigValueSourceKind::Workspace, &document.path);
    }
    if workspace.is_some() && WORKSPACE_REPLACE_ONLY_KEYS.contains(&key) {
        return built_in_source();
    }
    // A workspace preset selection resets the preset-managed keys: the global
    // explicit value did not survive the merge, so it is not the source.
    if PRESET_MANAGED_KEYS.contains(&key)
        && workspace
            .is_some_and(|document| value_at_path(&document.value, OperationPreset::KEY).is_some())
    {
        return built_in_source();
    }
    if let Some(document) = global
        && value_at_path(&document.value, key).is_some()
    {
        return file_source(ConfigValueSourceKind::Global, &document.path);
    }
    if key == "workflow.default_crew"
        && std::env::var("CONSTELLATION_DEFAULT_PROVIDER")
            .is_ok_and(|value| !value.trim().is_empty())
    {
        return ConfigValueSource {
            kind: ConfigValueSourceKind::Environment,
            path: None,
        };
    }
    built_in_source()
}

fn source_for_crew_field(
    crew: &str,
    field: &str,
    global: Option<&ConfigDocument>,
    workspace: Option<&ConfigDocument>,
) -> ConfigValueSource {
    for (kind, document) in [
        (ConfigValueSourceKind::Workspace, workspace),
        (ConfigValueSourceKind::Global, global),
    ] {
        if let Some(document) = document
            && let Some(entry) = crew_entry(&document.value, crew)
            && entry.contains_key(field)
        {
            return file_source(kind, &document.path);
        }
    }
    built_in_source()
}

fn file_source(kind: ConfigValueSourceKind, path: &Path) -> ConfigValueSource {
    ConfigValueSource {
        kind,
        path: Some(path.to_path_buf()),
    }
}

fn built_in_source() -> ConfigValueSource {
    ConfigValueSource {
        kind: ConfigValueSourceKind::BuiltIn,
        path: None,
    }
}
