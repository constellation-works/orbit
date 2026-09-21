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
//! - the `[machine]` table is global-only: a workspace file that supplies it
//!   is refused before the merge, so a checkout can never rename, renumber, or
//!   re-identify the machine it happens to be checked out on;
//! - a crew name containing `:` is refused per layer, before the merge, so the
//!   error names the file that defines it — the only way back from a persisted
//!   colon-named crew is editing that file;
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
use crate::crew_pools::reject_unpoolable_crew_names_in_document;
use crate::operation::{
    OperationLayer, OperationLayerSource, OperationPolicy, OperationPreset, PRESET_MANAGED_KEYS,
};
use crate::persistence::PersistenceConfig;
use crate::registry::{CONFIG_KEY_REGISTRY, GLOBAL_ONLY_KEY_PREFIX};
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

/// Why a layer that defines a key did not supply the effective value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowReason {
    /// A higher layer set the same key.
    Overridden,
    /// A security key ([`WORKSPACE_REPLACE_ONLY_KEYS`]) that the workspace
    /// file must restate to keep: it never inherits from global once a
    /// distinct workspace file exists.
    NotInherited,
    /// A workspace `operation.preset` selection reset this preset-managed
    /// key, so the global explicit value did not survive the merge.
    PresetReset,
}

impl ShadowReason {
    /// Stable token used in `orbit config show --json`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Overridden => "overridden",
            Self::NotInherited => "not-inherited",
            Self::PresetReset => "preset-reset",
        }
    }
}

/// A layer that defines a key without supplying the effective value.
///
/// This is what makes the two surprising cases legible in `config show`: a
/// global value a workspace overrode, and a global security value that was
/// deliberately not inherited.
#[derive(Debug, Clone, PartialEq)]
pub struct ShadowedConfigValue {
    /// Layer that defines the shadowed value.
    pub layer: ConfigValueSourceKind,
    /// The value that layer defines, projected as JSON.
    pub value: serde_json::Value,
    /// Why it is not the effective value.
    pub reason: ShadowReason,
}

/// Three-way state of one resolved value.
///
/// `unset` and `default` are different facts that both used to render as
/// `[built-in]`: one setting has no value at all, the other has a compiled-in
/// one that is actually in force.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigValueState {
    /// A config file (or the environment) supplied the value.
    Set,
    /// No file supplies it; the compiled-in default is in force.
    Default,
    /// No file supplies it and there is no default: the key has no value.
    Unset,
}

impl ConfigValueState {
    /// Stable token used in `orbit config show --json`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Default => "default",
            Self::Unset => "unset",
        }
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
    /// Layers that define the key without supplying the effective value,
    /// highest-precedence first. Empty for the ordinary case.
    pub shadowed_by: Vec<ShadowedConfigValue>,
}

impl EffectiveConfigValue {
    /// Whether the value is set by a layer, defaulted, or absent entirely.
    pub fn state(&self) -> ConfigValueState {
        match self.source.kind() {
            ConfigValueSourceKind::BuiltIn if self.value.is_null() => ConfigValueState::Unset,
            ConfigValueSourceKind::BuiltIn => ConfigValueState::Default,
            _ => ConfigValueState::Set,
        }
    }
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

    // Merging erases which file a crew came from, and a colon-named crew is
    // only repairable by hand-editing that file, so each layer is checked
    // while its own path is still known.
    for document in [global.as_ref(), workspace.as_ref()].into_iter().flatten() {
        reject_unpoolable_crew_names_in_document(&document.value, &document.path)?;
    }
    if let Some(workspace_document) = &workspace {
        reject_workspace_machine_table(&workspace_document.value, &workspace_document.path)?;
    }

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
    let mut resolved = ResolvedConfig::from_layered_value(merged, config_path, persistence)?;
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

/// Refuse a `[machine]` table in a workspace `config.toml`.
///
/// Machine identity is a per-user, per-machine fact: exactly the kind of value
/// a checkout must not be able to supply or override. This is the mirror image
/// of the replace-only security keys — there the workspace layer may set the
/// value and must restate it to keep it, here it may not set it at all.
pub(crate) fn reject_workspace_machine_table(
    document: &toml::Value,
    path: &Path,
) -> Result<(), OrbitError> {
    let table = GLOBAL_ONLY_KEY_PREFIX.trim_end_matches('.');
    if value_at_path(document, table).is_none() {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "[{table}] is not a workspace setting: remove it from '{}'. This machine's identity \
         lives only in the global config.toml, where `orbit init` writes it; \
         `orbit config set --global machine.name <value>` renames it",
        redact_home_dir(&path.display().to_string())
    )))
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
        .map(|(key, value)| {
            let source = source_for_key(key, global, workspace);
            EffectiveConfigValue {
                shadowed_by: shadowed_for_key(key, &source, global, workspace),
                key: key.to_string(),
                value,
                source,
            }
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
            let source = source_for_crew_field(name, field, global, workspace);
            values.push(EffectiveConfigValue {
                shadowed_by: shadowed_for_crew_field(name, field, &source, global),
                source,
                key,
                value,
            });
        }
    }
    values.sort_by(|left, right| left.key.cmp(&right.key));
    values
}

/// Layers that define `key` without supplying the effective value.
///
/// Only the global layer can be shadowed today: it is the one layer below a
/// workspace file, and the two non-inheriting rules ([`WORKSPACE_REPLACE_ONLY_KEYS`]
/// and the workspace preset reset) both drop a global value.
fn shadowed_for_key(
    key: &str,
    source: &ConfigValueSource,
    global: Option<&ConfigDocument>,
    workspace: Option<&ConfigDocument>,
) -> Vec<ShadowedConfigValue> {
    if source.kind() == ConfigValueSourceKind::Global {
        return Vec::new();
    }
    let Some(document) = global else {
        return Vec::new();
    };
    let Some(value) = value_at_path(&document.value, key) else {
        return Vec::new();
    };
    // Mirrors the order `source_for_key` refuses the global value in, so the
    // stated reason is the rule that actually applied.
    let reason = if source.kind() == ConfigValueSourceKind::Workspace {
        ShadowReason::Overridden
    } else if workspace.is_some() && WORKSPACE_REPLACE_ONLY_KEYS.contains(&key) {
        ShadowReason::NotInherited
    } else if PRESET_MANAGED_KEYS.contains(&key) {
        ShadowReason::PresetReset
    } else {
        ShadowReason::Overridden
    };
    vec![ShadowedConfigValue {
        layer: ConfigValueSourceKind::Global,
        value: json_from_toml(value),
        reason,
    }]
}

/// The global definition of a crew field a workspace crew table overrode.
fn shadowed_for_crew_field(
    crew: &str,
    field: &str,
    source: &ConfigValueSource,
    global: Option<&ConfigDocument>,
) -> Vec<ShadowedConfigValue> {
    if source.kind() != ConfigValueSourceKind::Workspace {
        return Vec::new();
    }
    let Some(value) = global
        .and_then(|document| crew_entry(&document.value, crew))
        .and_then(|entry| entry.get(field))
    else {
        return Vec::new();
    };
    vec![ShadowedConfigValue {
        layer: ConfigValueSourceKind::Global,
        value: json_from_toml(value),
        reason: ShadowReason::Overridden,
    }]
}

/// Project a TOML value as JSON for display. A value that will not project
/// (only TOML datetimes, which no config key uses) renders as null rather
/// than failing a read-only listing.
fn json_from_toml(value: &toml::Value) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or(serde_json::Value::Null)
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
