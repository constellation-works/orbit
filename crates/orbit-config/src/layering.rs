//! Layered resolution and source provenance.
//!
//! Reads the global and workspace documents, merges them per key into one
//! document that [`crate::resolved`] admits, and answers "where did this value
//! come from?" for `orbit config show`/`get`.
//!
//! These layering rules live here and nowhere else:
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
//! - `[plugin_enablement]` is workspace-only and is lifted off the workspace
//!   file before the merge ([`crate::plugin_enablement`]), so a file holding
//!   only plugin toggles does not count as a distinct workspace file for the
//!   replace-only keys.
//!
//! The review keys are resolved per layer by [`crate::operation`] rather than
//! from the merged document, so their provenance is exact. Each document's
//! deprecated review keys are translated to their `[review]` spelling as it
//! is parsed, so the merge, the snapshot and `orbit config get` all see the
//! translated value.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_types::identity::Crew;

use crate::ConfigRoots;
use crate::crew_pools::reject_unpoolable_crew_names_in_document;
use crate::operation::{
    OperationLayer, OperationLayerSource, OperationPolicy, translate_legacy_review_keys,
};
use crate::persistence::PersistenceConfig;
use crate::plugin_enablement::{
    plugin_enablement_from_document, reject_global_plugin_enablement, strip_plugin_enablement,
};
use crate::registry::{CONFIG_KEY_REGISTRY, ConfigSnapshot, GLOBAL_ONLY_KEY_PREFIX};
use crate::resolved::{ResolvedConfig, default_crews, warn_compatibility_keys};
use crate::store::read_config_file;

/// Security-sensitive settings that a workspace file must restate to keep.
/// Inheriting a machine-global sandbox, approval, or environment allowlist
/// into a workspace that never asked for it is the failure mode this prevents.
const WORKSPACE_REPLACE_ONLY_KEYS: &[&str] = &[
    "execution.codex.approval_policy",
    "execution.codex.sandbox",
    "execution.env.pass",
];

/// The one key an environment variable can supply, below both files.
const DEFAULT_CREW_KEY: &str = "workflow.default_crew";
const DEFAULT_PROVIDER_ENV: &str = "CONSTELLATION_DEFAULT_PROVIDER";

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
    /// A security key (`WORKSPACE_REPLACE_ONLY_KEYS`) that the workspace
    /// file must restate to keep: it never inherits from global once a
    /// distinct workspace file exists.
    NotInherited,
}

impl ShadowReason {
    /// Stable token used in `orbit config show --json`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Overridden => "overridden",
            Self::NotInherited => "not-inherited",
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

/// The layer and value a key resolves to once the layer that supplies it
/// stops setting it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigFallback {
    /// Layer that would supply the value.
    pub layer: ConfigValueSourceKind,
    /// The value that layer would supply, projected as JSON. Null when it is
    /// the built-in layer and the key has no value there.
    pub value: serde_json::Value,
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
    /// The global value, admitted for registry keys in the effective crew/settings context,
    /// including when shadowed. `None` when the global file does not set the
    /// key or its value cannot be admitted in that context. `shadowed_by`
    /// retains the raw file value for provenance compatibility.
    pub global_value: Option<serde_json::Value>,
    /// The value Orbit ships with, before any file, the environment, or a
    /// configured crew registry; JSON null when the key has none. A crew
    /// field reports the built-in crew of that name. A plugin key reports
    /// null: its defaults belong to the plugin.
    pub built_in: serde_json::Value,
    /// What the key resolves to if the layer in `source` stopped setting it.
    /// A replace-only security key the workspace sets falls to the built-in
    /// value, never the global one, because the workspace file still exists.
    /// `None` for a built-in value, which no layer can remove.
    pub fallback: Option<ConfigFallback>,
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
        if let Ok(Some(parsed)) = crate::plugins::parse_plugin_field_key(key) {
            let prefix = format!("plugins.{}.", parsed.namespace);
            if self
                .values
                .iter()
                .any(|entry| entry.key.starts_with(&prefix))
            {
                return Some(serde_json::Value::Null);
            }
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
    load_layered_resolved_with_staged(roots, None, None)
}

/// Admit an in-memory workspace edit against the current global layer before
/// the edited document is written. This uses the same merge and per-layer
/// checks as a normal load, including validation of plugin toggles.
pub(crate) fn validate_staged_workspace_document(
    roots: &ConfigRoots,
    workspace_path: &Path,
    raw: &str,
) -> Result<ResolvedConfig, OrbitError> {
    if !roots.has_workspace_layer() {
        return Err(OrbitError::InvalidInput(format!(
            "`--root` or `ORBIT_ROOT` pins one config root for both layers, so there is no \
             workspace config layer; rerun with `--global` to edit '{}'",
            redact_home_dir(&roots.global().join("config.toml").display().to_string())
        )));
    }
    load_layered_resolved_with_staged(roots, None, Some((workspace_path, raw)))
        .map(|loaded| loaded.resolved)
}

/// Admit an in-memory global edit against the workspace layer it would be
/// read with, through the same layered load a runtime performs. Every
/// admission rule that spans both layers applies (both review layers on,
/// resource-throttle resume against high, crews a workspace pool or
/// `default_crew` names). Without this a global edit that is valid alone saves
/// cleanly and leaves that workspace's config unloadable, including for the
/// `config set` that would repair it.
pub(crate) fn validate_staged_global_document(
    roots: &ConfigRoots,
    global_path: &Path,
    raw: &str,
) -> Result<(), OrbitError> {
    if !roots.has_workspace_layer() {
        return Ok(());
    }
    load_layered_resolved_with_staged(roots, Some((global_path, raw)), None).map(|_| ())
}

/// Resolve workspace file values with only crew definitions inherited from
/// global. Other global settings must not appear in a scoped file snapshot.
pub(crate) fn resolve_workspace_file_document(
    global_path: &Path,
    global_raw: &str,
    workspace_path: &Path,
    raw: &str,
) -> Result<ResolvedConfig, OrbitError> {
    let workspace = parse_config_document(workspace_path, raw)?;
    reject_workspace_machine_table(&workspace.value, workspace_path)?;
    let global = parse_config_document(global_path, global_raw)?;
    for document in [&global, &workspace] {
        reject_unpoolable_crew_names_in_document(&document.value, &document.path)?;
    }

    let mut table = toml::map::Map::new();
    if let Some(crews) = global.value.get("crews") {
        table.insert("crews".to_string(), crews.clone());
    }
    let mut scoped = toml::Value::Table(table);
    // The normal recursive merge preserves global fields when the workspace
    // overrides only part of a crew definition.
    merge_tables(&mut scoped, &workspace.value);
    warn_compatibility_keys(&workspace.value, workspace_path);
    let crew_field_path = |crew: &str, field: &str| {
        source_for_crew_field(crew, field, Some(&global), Some(&workspace))
            .path
            .unwrap_or_else(|| workspace_path.to_path_buf())
    };
    ResolvedConfig::from_scoped_value(
        scoped,
        workspace_path,
        &crew_field_path,
        PersistenceConfig::default_for_data_root(workspace_path.parent().unwrap_or(workspace_path)),
    )
}

/// The layered load, with an in-memory global or workspace document standing
/// in for the file it will be written to.
fn load_layered_resolved_with_staged(
    roots: &ConfigRoots,
    staged_global: Option<(&Path, &str)>,
    staged_workspace: Option<(&Path, &str)>,
) -> Result<LoadedResolvedConfig, OrbitError> {
    let global = if let Some((path, raw)) = staged_global {
        Some(parse_config_document(path, raw)?)
    } else {
        read_config_document(&roots.global().join("config.toml"))?
    };
    if let Some(global_document) = &global {
        reject_global_plugin_enablement(&global_document.value, &global_document.path)?;
    }
    let mut workspace = if roots.has_workspace_layer() {
        if let Some((path, raw)) = staged_workspace {
            Some(parse_config_document(path, raw)?)
        } else {
            read_config_document(&roots.workspace().join("config.toml"))?
        }
    } else {
        None
    };
    // The plugin toggles are read off the workspace file before it layers:
    // they are not a config setting, and a file that holds only them is not a
    // policy layer — it must not switch the replace-only security keys away
    // from global.
    let mut plugin_enablement = BTreeMap::new();
    if let Some(mut document) = workspace.take() {
        plugin_enablement = plugin_enablement_from_document(&document.value, &document.path)?;
        if strip_plugin_enablement(&mut document.value) {
            workspace = Some(document);
        }
    }
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
        let mut resolved = ResolvedConfig::built_in(persistence);
        resolved.plugin_enablement = plugin_enablement;
        return Ok(LoadedResolvedConfig {
            resolved,
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
    }

    let config_path = workspace
        .as_ref()
        .or(global.as_ref())
        .map(|document| document.path.as_path())
        .unwrap_or_else(|| Path::new("<built-in defaults>"));
    // The merged document has lost which layer set each crew field, so the
    // ignored-field records ask the layers directly, as `config show` does.
    let crew_field_path = |crew: &str, field: &str| {
        source_for_crew_field(crew, field, global.as_ref(), workspace.as_ref())
            .path
            .unwrap_or_else(|| config_path.to_path_buf())
    };
    let mut resolved =
        ResolvedConfig::from_layered_value(merged, config_path, &crew_field_path, persistence)?;
    for document in [global.as_ref(), workspace.as_ref()].into_iter().flatten() {
        warn_compatibility_keys(&document.value, &document.path);
    }
    resolved.operation = resolve_operation_layers(global.as_ref(), workspace.as_ref())?;
    // The winning layer's pool is the merged one, which the snapshot has
    // admitted against the crew registry; keep its canonical entries.
    resolved.operation.review_crew.value = resolved
        .snapshot
        .operation_review_crew
        .clone()
        .unwrap_or_default();
    resolved.plugin_enablement = plugin_enablement;
    Ok(LoadedResolvedConfig {
        resolved,
        global,
        workspace,
    })
}

/// Resolve the review preferences from the exact layers rather than the
/// merged document, so each field records the layer that set it.
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
    let policy = OperationPolicy::resolve(&[
        (OperationLayerSource::Global, &global_layer),
        (OperationLayerSource::Workspace, &workspace_layer),
    ]);
    policy.ensure_one_review_layer()?;
    Ok(policy)
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
         and worker limits live only in the global config.toml, where `orbit init` writes the \
         identity; `orbit config set --global machine.<key> <value>` edits them",
        redact_home_dir(&path.display().to_string())
    )))
}

fn read_config_document(path: &Path) -> Result<Option<ConfigDocument>, OrbitError> {
    let Some(raw) = read_config_file(path)? else {
        return Ok(None);
    };
    parse_config_document(path, &raw).map(Some)
}

fn parse_config_document(path: &Path, raw: &str) -> Result<ConfigDocument, OrbitError> {
    let mut value = toml::from_str(raw).map_err(|err| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {err}",
            redact_home_dir(&path.display().to_string())
        ))
    })?;
    translate_legacy_review_keys(&mut value, path)?;
    if let Some(patterns) =
        crate::registry::read_optional::<Vec<String>>(&value, "worktree.reclaim", path)?
    {
        for pattern in patterns {
            orbit_common::fs::path_glob::RelativePathGlob::new(&pattern)
                .map_err(|error| OrbitError::InvalidInput(format!("worktree.reclaim: {error}")))?;
        }
    }
    Ok(ConfigDocument {
        path: path.to_path_buf(),
        value,
    })
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

pub(crate) fn set_value_at_path(document: &mut toml::Value, key: &str, value: toml::Value) {
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
    let below = BelowFiles::new(&resolved.crews);
    let mut values = resolved
        .snapshot
        .all_values()
        .into_iter()
        .map(|(key, value)| {
            let source = source_for_key(key, global, workspace);
            let global_value = global.and_then(|document| {
                value_at_path(&document.value, key)?;
                if source.kind() == ConfigValueSourceKind::Global {
                    return Some(value.clone());
                }
                resolved
                    .snapshot
                    .admit_override(&document.value, key, &document.path, &resolved.crews)
                    .ok()
                    .flatten()
            });
            EffectiveConfigValue {
                shadowed_by: shadowed_for_key(key, &source, global, workspace),
                built_in: below.shipped_value(key),
                fallback: fallback_for_key(key, &source, global, global_value.as_ref(), &below),
                global_value,
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
        for (field, value) in crew_field_values(crew) {
            let key = format!("crews.{name}.{field}");
            let source = source_for_crew_field(name, field, global, workspace);
            let built_in = below.shipped_crew_field(name, field);
            values.push(EffectiveConfigValue {
                shadowed_by: shadowed_for_crew_field(name, field, &source, global),
                fallback: fallback_for_crew_field(name, field, &source, global, &built_in),
                global_value: global
                    .and_then(|document| crew_entry(&document.value, name))
                    .and_then(|entry| entry.get(field))
                    .map(json_from_toml),
                built_in,
                source,
                key,
                value,
            });
        }
    }
    // `[plugins.<ns>]` rows layer like crews: one row per key actually present
    // in a layer, with the file that supplied it. A key the plugin defaults
    // but no file sets is reported by `orbit plugin show`, not here — this
    // list is what the config files say.
    for (namespace, section) in &resolved.plugins {
        let Some(entries) = section.as_object() else {
            continue;
        };
        for (field, value) in entries {
            let key = format!("plugins.{namespace}.{field}");
            let source = source_for_key(&key, global, workspace);
            let global_value = global
                .and_then(|document| value_at_path(&document.value, &key))
                .map(json_from_toml);
            values.push(EffectiveConfigValue {
                shadowed_by: shadowed_for_key(&key, &source, global, workspace),
                built_in: serde_json::Value::Null,
                fallback: fallback_for_key(&key, &source, global, global_value.as_ref(), &below),
                global_value,
                source,
                key,
                value: value.clone(),
            });
        }
    }
    values.sort_by(|left, right| left.key.cmp(&right.key));
    values
}

/// A crew's projected fields, as `crews.<name>.<field>` rows list them.
fn crew_field_values(crew: &Crew) -> Vec<(&'static str, serde_json::Value)> {
    let mut fields = vec![
        ("model", serde_json::json!(crew.assignment.model)),
        ("provider", serde_json::json!(crew.assignment.provider)),
        ("description", serde_json::json!(crew.description)),
        ("tags", serde_json::json!(crew.tags)),
        // Always projected: a table without the key is enabled, and the
        // listing shows that rather than hiding the state.
        ("enabled", serde_json::json!(crew.enabled)),
    ];
    // Configured effort only. An omitted field keeps the provider default
    // and must not appear as a fabricated effective setting.
    if let Some(effort) = crew.assignment.effort {
        fields.push(("effort", serde_json::json!(effort)));
    }
    fields
}

/// What a key resolves to when neither file sets it.
struct BelowFiles {
    /// Shipped defaults: no file, no environment, the built-in crew registry.
    shipped: ConfigSnapshot,
    shipped_crews: BTreeMap<String, Crew>,
    /// Built-in values admitted against the crew registry the files define,
    /// which is what an unset key resolves to here: the default crew and the
    /// final-recovery pool depend on which crews exist. `None` when that
    /// admission fails, in which case the shipped value stands in.
    configured: Option<ConfigSnapshot>,
    /// `workflow.default_crew` as `CONSTELLATION_DEFAULT_PROVIDER` selects it.
    environment_default_crew: Option<serde_json::Value>,
}

impl BelowFiles {
    fn new(crews: &BTreeMap<String, Crew>) -> Self {
        let empty = empty_document();
        let path = Path::new("<built-in defaults>");
        let admit = |env_default: Option<&str>| {
            ConfigSnapshot::admit_with_env(&empty, path, crews, env_default, false).ok()
        };
        let environment_default_crew = environment_default_provider()
            .and_then(|provider| admit(Some(&provider)))
            .and_then(|snapshot| snapshot.value_for(DEFAULT_CREW_KEY));
        Self {
            shipped: ConfigSnapshot::default(),
            shipped_crews: default_crews(),
            configured: admit(None),
            environment_default_crew,
        }
    }

    fn shipped_value(&self, key: &str) -> serde_json::Value {
        self.shipped
            .value_for(key)
            .unwrap_or(serde_json::Value::Null)
    }

    fn shipped_crew_field(&self, crew: &str, field: &str) -> serde_json::Value {
        self.shipped_crews
            .get(crew)
            .and_then(|crew| {
                crew_field_values(crew)
                    .into_iter()
                    .find(|(name, _)| *name == field)
            })
            .map_or(serde_json::Value::Null, |(_, value)| value)
    }

    fn built_in(&self, key: &str) -> ConfigFallback {
        ConfigFallback {
            layer: ConfigValueSourceKind::BuiltIn,
            value: self
                .configured
                .as_ref()
                .unwrap_or(&self.shipped)
                .value_for(key)
                .unwrap_or(serde_json::Value::Null),
        }
    }

    /// The environment for the one key it can supply, else the built-in value,
    /// in the order `source_for_key` consults them.
    fn below_files(&self, key: &str) -> ConfigFallback {
        match &self.environment_default_crew {
            Some(value) if key == DEFAULT_CREW_KEY => ConfigFallback {
                layer: ConfigValueSourceKind::Environment,
                value: value.clone(),
            },
            _ => self.built_in(key),
        }
    }
}

/// Where `key` resolves once the layer in `source` stops setting it.
///
/// Mirrors `source_for_key`: removing a workspace value reaches the global
/// one, except for a replace-only key, which never inherits while the
/// workspace file exists — and it still exists, since it set this key.
fn fallback_for_key(
    key: &str,
    source: &ConfigValueSource,
    global: Option<&ConfigDocument>,
    global_value: Option<&serde_json::Value>,
    below: &BelowFiles,
) -> Option<ConfigFallback> {
    match source.kind() {
        ConfigValueSourceKind::BuiltIn => None,
        ConfigValueSourceKind::Environment => Some(below.built_in(key)),
        ConfigValueSourceKind::Global => Some(below.below_files(key)),
        ConfigValueSourceKind::Workspace if WORKSPACE_REPLACE_ONLY_KEYS.contains(&key) => {
            Some(below.built_in(key))
        }
        ConfigValueSourceKind::Workspace => {
            if global
                .and_then(|document| value_at_path(&document.value, key))
                .is_some()
            {
                global_value.map(|value| ConfigFallback {
                    layer: ConfigValueSourceKind::Global,
                    value: value.clone(),
                })
            } else {
                Some(below.below_files(key))
            }
        }
    }
}

/// Where a crew field resolves once the layer in `source` stops setting it:
/// the global crew table's field under a workspace one, else the built-in
/// crew's field.
fn fallback_for_crew_field(
    crew: &str,
    field: &str,
    source: &ConfigValueSource,
    global: Option<&ConfigDocument>,
    built_in: &serde_json::Value,
) -> Option<ConfigFallback> {
    let global_value = global
        .and_then(|document| crew_entry(&document.value, crew))
        .and_then(|entry| entry.get(field));
    match (source.kind(), global_value) {
        (ConfigValueSourceKind::BuiltIn, _) => None,
        (ConfigValueSourceKind::Workspace, Some(value)) => Some(ConfigFallback {
            layer: ConfigValueSourceKind::Global,
            value: json_from_toml(value),
        }),
        _ => Some(ConfigFallback {
            layer: ConfigValueSourceKind::BuiltIn,
            value: built_in.clone(),
        }),
    }
}

fn environment_default_provider() -> Option<String> {
    std::env::var(DEFAULT_PROVIDER_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Layers that define `key` without supplying the effective value.
///
/// Only the global layer can be shadowed today: it is the one layer below a
/// workspace file, and the non-inheriting rule ([`WORKSPACE_REPLACE_ONLY_KEYS`])
/// drops a global value.
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
    if let Some(document) = global
        && value_at_path(&document.value, key).is_some()
    {
        return file_source(ConfigValueSourceKind::Global, &document.path);
    }
    if key == DEFAULT_CREW_KEY && environment_default_provider().is_some() {
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
