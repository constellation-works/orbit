//! Structured `config.toml` views and edits for non-CLI surfaces [ORB-12724].
//!
//! `orbit config show` renders the layered view as text; the dashboard needs
//! the same facts as data. Everything here is a projection of what
//! [`orbit_config`] already resolved — the layering, the three-way value
//! state, the shadowed lower layers, and the registry's section/description/
//! choice metadata. Provenance is never re-derived from the raw documents,
//! so the dashboard and the CLI cannot disagree about where a value came
//! from.
//!
//! Writes go through the same [`orbit_config::ConfigStore`] admission path as
//! `orbit config set`, so a refused value is refused identically here and the
//! caller can render the admission error verbatim.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;
use orbit_config::{
    CONFIG_KEY_REGISTRY, ConfigKeyDescriptor, ConfigRoots, ConfigSection, ConfigStore,
    ConfigValueSourceKind, ConfigValueState, EffectiveConfigValue, ShadowReason, WorkspaceInitMode,
    admit_config_key, config_key_options, describe_config_key, load_effective_config,
};
use serde_json::{Map, Value as JsonValue, json};

use crate::runtime::{CONFIG_TOML_FILE, OrbitRuntime, existing_config_file_path};

/// Which physical `config.toml` a view or a write targets. Re-exported so a
/// transport can name the scope without depending on `orbit-config` directly.
pub use orbit_config::ConfigScope;

/// Fields a crew table carries, in the order a crew row renders them.
const CREW_FIELDS: &[&str] = &["provider", "model", "effort", "tags", "description"];

/// `workflow.*` keys that name a crew by name. Deleting the crew they point at
/// leaves the workspace unable to resolve work, so the delete is refused with
/// the key that still names it.
const CREW_REFERENCE_KEYS: &[&str] = &["workflow.default_crew", "workflow.system_crew"];

/// How a write initializes a workspace `config.toml` that does not exist yet.
///
/// The default is fail-closed for the same reason `orbit config set` is: the
/// first workspace file switches the security-sensitive `execution.*` keys
/// away from global policy, so it is never created as a side effect of an
/// unrelated edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConfigWriteInit {
    #[default]
    RequireExisting,
    SeedFromGlobal,
    Fresh,
}

impl ConfigWriteInit {
    /// Parse the wire token a caller sends, or `None` for an unknown one.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "" | "require-existing" => Some(Self::RequireExisting),
            "seed-from-global" => Some(Self::SeedFromGlobal),
            "fresh" => Some(Self::Fresh),
            _ => None,
        }
    }

    fn mode(self) -> WorkspaceInitMode {
        match self {
            Self::RequireExisting => WorkspaceInitMode::RequireExisting,
            Self::SeedFromGlobal => WorkspaceInitMode::SeedFromGlobal,
            Self::Fresh => WorkspaceInitMode::Fresh,
        }
    }
}

/// What one accepted write changed, for the caller's audit record and for the
/// row it re-renders.
#[derive(Debug, Clone)]
pub struct ConfigWriteOutcome {
    /// Value in force before the write, as JSON.
    pub old_value: JsonValue,
    /// Value in force after it.
    pub new_value: JsonValue,
    /// `global` or `workspace`: the file that was written.
    pub scope: &'static str,
    /// The written file.
    pub path: PathBuf,
    /// The re-resolved rows for the keys this write touched.
    pub rows: Vec<JsonValue>,
}

/// Parse the `scope` a caller selects for a file view or a write.
pub fn parse_config_scope(raw: &str) -> Option<ConfigScope> {
    match raw.trim() {
        "global" => Some(ConfigScope::Global),
        "workspace" => Some(ConfigScope::Workspace),
        _ => None,
    }
}

/// The layered view: sections with their rows, the layer paths, the workspace
/// registry binding, resolved paths, and the crew table.
pub fn effective_view(runtime: &OrbitRuntime) -> Result<JsonValue, OrbitError> {
    let effective = load_effective_config(&config_roots(runtime))?;
    let values = effective.values();
    let global_file = config_layer_file(&runtime.global_root())?;
    let workspace_file = config_layer_file(&runtime.shared_root())?;
    let workspace_file_exists = workspace_file.exists;

    let sections = effective_sections(values);
    Ok(json!({
        "scope": "effective",
        "layers": {
            "built_in": {"label": ConfigValueSourceKind::BuiltIn.label()},
            "global": global_file.json(),
            "workspace": workspace_file.json(),
            // The security exception is only in force once a workspace file
            // exists; before that, global values still apply and warning about
            // them would be wrong.
            "execution_not_inherited": workspace_file_exists,
            "not_inherited_keys": not_inherited_keys(values),
        },
        "workspace_binding": workspace_binding_json(runtime, values),
        "sections": sections,
        "crews": crew_rows(values),
        "paths": path_rows_json(runtime, None),
        "crew_fields": CREW_FIELDS,
        "write_scope_default": ConfigScope::Workspace.label(),
        "workspace_file_exists": workspace_file_exists,
    }))
}

/// One physical file resolved in isolation, mirroring `orbit config show
/// --scope global|workspace`: the same grouping without layering, shadowed
/// values, or crews (a scoped snapshot admits registry keys only).
pub fn file_view(runtime: &OrbitRuntime, scope: ConfigScope) -> Result<JsonValue, OrbitError> {
    let global_file = config_layer_file(&runtime.global_root())?;
    let workspace_file = config_layer_file(&runtime.shared_root())?;
    let file = match scope {
        ConfigScope::Global => &global_file,
        ConfigScope::Workspace => &workspace_file,
    };
    let store = ConfigStore::open(scope, file.path.clone())?;
    let snapshot = store.snapshot()?;
    let settings = snapshot.all_values();
    let sections = file_sections(&store, &settings);

    Ok(json!({
        "scope": scope.label(),
        "file": file.json(),
        "layers": {
            "built_in": {"label": ConfigValueSourceKind::BuiltIn.label()},
            "global": global_file.json(),
            "workspace": workspace_file.json(),
            "execution_not_inherited": workspace_file.exists,
            "not_inherited_keys": JsonValue::Array(Vec::new()),
        },
        "workspace_binding": JsonValue::Null,
        "sections": sections,
        // Crew tables are not registry keys, so a single-file snapshot cannot
        // enumerate them; the effective view is where crews are shown.
        "crews": JsonValue::Array(Vec::new()),
        "paths": path_rows_json(runtime, Some(store.path())),
        "crew_fields": CREW_FIELDS,
        "write_scope_default": scope.label(),
        "workspace_file_exists": workspace_file.exists,
    }))
}

/// The `orbit config keys` reference: every settable key with its type,
/// section, description, and accepted choices.
pub fn key_catalog() -> JsonValue {
    let keys = CONFIG_KEY_REGISTRY
        .iter()
        .map(|descriptor| {
            json!({
                "key": descriptor.key,
                "value_type": descriptor.value_type,
                "options": config_key_options(descriptor.key),
                "section": descriptor.section.token(),
                "section_title": descriptor.section.title(),
                "description": descriptor.description,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "keys": keys,
        "sections": section_catalog(),
        "crew_fields": CREW_FIELDS,
    })
}

/// Write one key into `scope`'s file, admitting it exactly as `orbit config
/// set` would.
pub fn set_key(
    runtime: &OrbitRuntime,
    key: &str,
    value: &JsonValue,
    scope: ConfigScope,
    init: ConfigWriteInit,
) -> Result<ConfigWriteOutcome, OrbitError> {
    admit_config_key(key)?;
    let old_value = effective_value(runtime, key)?;
    let mut store = open_store_for_write(runtime, scope, init)?;
    store.set_value(key, &toml_literal(value)?)?;
    store.validate()?;
    store.save()?;
    write_outcome(runtime, scope, &store, old_value, &[key.to_string()])
}

/// Clear one key from `scope`'s file so the layer below takes over again.
pub fn unset_key(
    runtime: &OrbitRuntime,
    key: &str,
    scope: ConfigScope,
) -> Result<ConfigWriteOutcome, OrbitError> {
    admit_config_key(key)?;
    let old_value = effective_value(runtime, key)?;
    let mut store = open_store_for_write(runtime, scope, ConfigWriteInit::RequireExisting)?;
    if !store.unset_value(key)? {
        return Err(OrbitError::InvalidInput(format!(
            "'{key}' is not set in the {} config; nothing to clear",
            scope.label()
        )));
    }
    store.validate()?;
    store.save()?;
    write_outcome(runtime, scope, &store, old_value, &[key.to_string()])
}

/// Create or edit one crew table. Only the supplied fields are written; a
/// field whose value is JSON null is cleared.
pub fn set_crew(
    runtime: &OrbitRuntime,
    name: &str,
    fields: &Map<String, JsonValue>,
    scope: ConfigScope,
    init: ConfigWriteInit,
) -> Result<ConfigWriteOutcome, OrbitError> {
    if fields.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "no crew fields supplied for '{name}'; expected one of: {}",
            CREW_FIELDS.join(", ")
        )));
    }
    let keys = fields
        .keys()
        .map(|field| format!("crews.{name}.{field}"))
        .collect::<Vec<_>>();
    for key in &keys {
        admit_config_key(key)?;
    }
    let old_value = crew_value(runtime, name)?;
    let mut store = open_store_for_write(runtime, scope, init)?;
    for (field, value) in fields {
        let key = format!("crews.{name}.{field}");
        if value.is_null() {
            store.unset_value(&key)?;
        } else {
            store.set_value(&key, &toml_literal(value)?)?;
        }
    }
    store.validate()?;
    store.save()?;
    write_outcome(runtime, scope, &store, old_value, &keys)
}

/// Delete one crew table, refusing while a `workflow.*` key still names it.
pub fn delete_crew(
    runtime: &OrbitRuntime,
    name: &str,
    scope: ConfigScope,
) -> Result<ConfigWriteOutcome, OrbitError> {
    let effective = load_effective_config(&config_roots(runtime))?;
    let referenced_by = CREW_REFERENCE_KEYS
        .iter()
        .filter(|key| {
            effective
                .value_for(key)
                .and_then(|value| value.as_str().map(str::to_string))
                .as_deref()
                == Some(name)
        })
        .copied()
        .collect::<Vec<_>>();
    if !referenced_by.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "crew '{name}' is named by {}; point {} at another crew before deleting it",
            referenced_by.join(" and "),
            if referenced_by.len() == 1 {
                "it"
            } else {
                "them"
            },
        )));
    }
    let old_value = crew_value(runtime, name)?;
    let mut store = open_store_for_write(runtime, scope, ConfigWriteInit::RequireExisting)?;
    if !store.remove_crew_table(name)? {
        return Err(OrbitError::InvalidInput(format!(
            "crew '{name}' is not defined in the {} config",
            scope.label()
        )));
    }
    store.validate()?;
    store.save()?;
    write_outcome(runtime, scope, &store, old_value, &[])
}

/// The resolved roots and store locations `orbit config show` prints under
/// `Paths`, as `(label, value)` rows in rendering order.
///
/// One authority for both surfaces: the CLI formats these into aligned
/// columns and the dashboard renders them as a two-column grid.
pub fn path_rows(runtime: &OrbitRuntime, config_path: Option<&Path>) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = vec![
        ("global root".to_string(), path_cell(&runtime.global_root())),
        (
            "workspace root".to_string(),
            path_cell(&runtime.shared_root()),
        ),
        ("local root".to_string(), path_cell(&runtime.local_root())),
    ];
    if let Some(config_path) = config_path {
        rows.push(("config file".to_string(), path_cell(config_path)));
    }
    rows.extend(persistence_rows(&runtime.persistence_config_json()));
    rows
}

/// Expand the persistence object into rows, folding resource directories that
/// share a parent into one row instead of repeating the prefix.
fn persistence_rows(persistence: &JsonValue) -> Vec<(String, String)> {
    const RESOURCE_LABELS: &[(&str, &str)] = &[
        ("activity", "activities"),
        ("executor", "executors"),
        ("job", "jobs"),
        ("policy", "policies"),
        ("skill", "skills"),
    ];
    const FILE_LABELS: &[(&str, &str)] = &[("audit", "audit db"), ("semantic", "semantic db")];

    let Some(entries) = persistence.as_object() else {
        return Vec::new();
    };
    let path_of = |name: &str| {
        entries
            .get(name)
            .and_then(|entry| entry.get("path"))
            .and_then(JsonValue::as_str)
            .map(PathBuf::from)
    };

    let mut rows = Vec::new();
    for (name, label) in FILE_LABELS {
        if let Some(path) = path_of(name) {
            rows.push(((*label).to_string(), path_cell(&path)));
        }
    }

    let mut grouped: BTreeMap<PathBuf, Vec<(&str, PathBuf)>> = BTreeMap::new();
    for (name, label) in RESOURCE_LABELS {
        if let Some(path) = path_of(name) {
            let parent = path.parent().unwrap_or(&path).to_path_buf();
            grouped.entry(parent).or_default().push((label, path));
        }
    }
    for (parent, members) in grouped {
        match members.as_slice() {
            [(label, path)] => rows.push(((*label).to_string(), path_cell(path))),
            members => rows.push((
                members
                    .iter()
                    .map(|(label, _)| *label)
                    .collect::<Vec<_>>()
                    .join(", "),
                format!("{}/", path_cell(&parent)),
            )),
        }
    }

    // Anything the persistence shape gains later is still reported, under its
    // own name, rather than silently dropped from this view.
    let known = RESOURCE_LABELS
        .iter()
        .chain(FILE_LABELS.iter())
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    for (name, entry) in entries {
        if known.contains(&name.as_str()) {
            continue;
        }
        let value = entry
            .get("path")
            .and_then(JsonValue::as_str)
            .map(|path| path_cell(Path::new(path)))
            .unwrap_or_else(|| entry.to_string());
        rows.push((name.clone(), value));
    }
    rows
}

fn path_rows_json(runtime: &OrbitRuntime, config_path: Option<&Path>) -> JsonValue {
    JsonValue::Array(
        path_rows(runtime, config_path)
            .into_iter()
            .map(|(label, value)| json!({"label": label, "value": value}))
            .collect(),
    )
}

fn config_roots(runtime: &OrbitRuntime) -> ConfigRoots {
    ConfigRoots::new(runtime.global_root(), runtime.shared_root())
}

fn global_config_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.global_root().join(CONFIG_TOML_FILE)
}

fn workspace_config_path(runtime: &OrbitRuntime) -> PathBuf {
    runtime.shared_root().join(CONFIG_TOML_FILE)
}

/// One physical `config.toml` layer: the path a caller sees and whether a
/// regular file is there.
///
/// Existence is probed through the runtime's validated config-root boundary
/// rather than a bare `Path::exists` on the joined path, so a root selected by
/// `?workspace=` never reaches a filesystem probe unvalidated (CodeQL
/// `rust/path-injection`). The displayed path stays the caller's spelling of
/// the root, not the canonical one, so it matches what `orbit config path`
/// prints.
struct ConfigLayerFile {
    path: PathBuf,
    exists: bool,
}

impl ConfigLayerFile {
    fn json(&self) -> JsonValue {
        json!({
            "path": path_cell(&self.path),
            "exists": self.exists,
        })
    }
}

fn config_layer_file(root: &Path) -> Result<ConfigLayerFile, OrbitError> {
    let exists = existing_config_file_path(root)?.is_some();
    Ok(ConfigLayerFile {
        path: root.join(CONFIG_TOML_FILE),
        exists,
    })
}

fn open_store_for_write(
    runtime: &OrbitRuntime,
    scope: ConfigScope,
    init: ConfigWriteInit,
) -> Result<ConfigStore, OrbitError> {
    match scope {
        ConfigScope::Global => ConfigStore::open(ConfigScope::Global, global_config_path(runtime)),
        ConfigScope::Workspace => ConfigStore::open_for_workspace_set(
            workspace_config_path(runtime),
            &global_config_path(runtime),
            init.mode(),
        ),
    }
}

/// The layered value of one key before a write, so the audit record can name
/// what the edit replaced.
fn effective_value(runtime: &OrbitRuntime, key: &str) -> Result<JsonValue, OrbitError> {
    Ok(load_effective_config(&config_roots(runtime))?
        .value_for(key)
        .unwrap_or(JsonValue::Null))
}

/// The layered value of one crew, as the object its row renders.
fn crew_value(runtime: &OrbitRuntime, name: &str) -> Result<JsonValue, OrbitError> {
    let effective = load_effective_config(&config_roots(runtime))?;
    Ok(crew_rows(effective.values())
        .into_iter()
        .find(|crew| crew["name"] == json!(name))
        .unwrap_or(JsonValue::Null))
}

fn write_outcome(
    runtime: &OrbitRuntime,
    scope: ConfigScope,
    store: &ConfigStore,
    old_value: JsonValue,
    keys: &[String],
) -> Result<ConfigWriteOutcome, OrbitError> {
    let effective = load_effective_config(&config_roots(runtime))?;
    let values = effective.values();
    let rows = keys
        .iter()
        .filter_map(|key| values.iter().find(|entry| &entry.key == key))
        .map(effective_row)
        .collect::<Vec<_>>();
    let new_value = match keys.first() {
        Some(key) if keys.len() == 1 => effective.value_for(key).unwrap_or(JsonValue::Null),
        Some(key) => {
            let name = key.split('.').nth(1).unwrap_or_default();
            crew_rows(values)
                .into_iter()
                .find(|crew| crew["name"] == json!(name))
                .unwrap_or(JsonValue::Null)
        }
        None => JsonValue::Null,
    };
    Ok(ConfigWriteOutcome {
        old_value,
        new_value,
        scope: scope.label(),
        path: store.path().to_path_buf(),
        rows,
    })
}

/// Render a JSON value as the TOML literal `ConfigStore::set_value` parses.
///
/// Typed input stays typed: a JSON string is quoted so a value like `"true"`
/// is written as a string rather than inferred as a boolean, and an array is
/// written as a TOML array rather than as its debug spelling.
fn toml_literal(value: &JsonValue) -> Result<String, OrbitError> {
    let literal: toml::Value = serde_json::from_value(value.clone()).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "value is not representable in config.toml: {error}"
        ))
    })?;
    Ok(literal.to_string())
}

/// Keys whose global value the workspace file did not inherit, so the caller
/// can badge the section that owns them.
fn not_inherited_keys(values: &[EffectiveConfigValue]) -> JsonValue {
    JsonValue::Array(
        values
            .iter()
            .filter(|entry| {
                entry
                    .shadowed_by
                    .iter()
                    .any(|shadow| shadow.reason == ShadowReason::NotInherited)
            })
            .map(|entry| json!(entry.key))
            .collect(),
    )
}

fn section_catalog() -> JsonValue {
    JsonValue::Array(
        ConfigSection::ORDER
            .iter()
            .map(|section| {
                json!({
                    "token": section.token(),
                    "title": section.title(),
                    "blurb": section.blurb(),
                    "key_prefix": section.key_prefix(),
                })
            })
            .collect(),
    )
}

fn effective_sections(values: &[EffectiveConfigValue]) -> JsonValue {
    JsonValue::Array(
        ConfigSection::ORDER
            .iter()
            .map(|section| {
                let rows = if *section == ConfigSection::Crews {
                    Vec::new()
                } else {
                    ordered_keys(*section)
                        .into_iter()
                        .filter_map(|descriptor| {
                            values.iter().find(|entry| entry.key == descriptor.key)
                        })
                        .map(effective_row)
                        .collect::<Vec<_>>()
                };
                section_json(*section, rows)
            })
            .collect(),
    )
}

fn file_sections(store: &ConfigStore, settings: &[(&'static str, JsonValue)]) -> JsonValue {
    JsonValue::Array(
        ConfigSection::ORDER
            .iter()
            .map(|section| {
                let rows = if *section == ConfigSection::Crews {
                    Vec::new()
                } else {
                    ordered_keys(*section)
                        .into_iter()
                        .filter_map(|descriptor| {
                            settings
                                .iter()
                                .find(|(key, _)| *key == descriptor.key)
                                .map(|(key, value)| file_row(store, descriptor, key, value))
                        })
                        .collect::<Vec<_>>()
                };
                section_json(*section, rows)
            })
            .collect(),
    )
}

fn section_json(section: ConfigSection, rows: Vec<JsonValue>) -> JsonValue {
    let count = |state: &str| {
        rows.iter()
            .filter(|row| row["state"] == json!(state))
            .count()
    };
    let not_inherited = rows
        .iter()
        .filter(|row| {
            row["shadowed_by"].as_array().is_some_and(|shadows| {
                shadows
                    .iter()
                    .any(|shadow| shadow["reason"] == json!("not-inherited"))
            })
        })
        .count();
    json!({
        "token": section.token(),
        "title": section.title(),
        "blurb": section.blurb(),
        "key_prefix": section.key_prefix(),
        "kind": if section == ConfigSection::Crews { "crews" } else { "keys" },
        "counts": {
            "set": count(ConfigValueState::Set.label()),
            "default": count(ConfigValueState::Default.label()),
            "unset": count(ConfigValueState::Unset.label()),
            "total": rows.len(),
        },
        "not_inherited": not_inherited,
        "keys": rows,
    })
}

fn effective_row(entry: &EffectiveConfigValue) -> JsonValue {
    let descriptor = describe_config_key(&entry.key);
    let mut row = base_row(&entry.key, descriptor, &entry.value);
    row.insert("state".to_string(), json!(entry.state().label()));
    row.insert(
        "source".to_string(),
        json!({
            "layer": entry.source.kind().label(),
            "path": entry.source.path().map(path_cell),
        }),
    );
    row.insert(
        "shadowed_by".to_string(),
        JsonValue::Array(
            entry
                .shadowed_by
                .iter()
                .map(|shadow| {
                    json!({
                        "layer": shadow.layer.label(),
                        "value": shadow.value,
                        "reason": shadow.reason.label(),
                        "note": shadow_note(shadow.layer, &shadow.value, shadow.reason),
                    })
                })
                .collect(),
        ),
    );
    JsonValue::Object(row)
}

fn file_row(
    store: &ConfigStore,
    descriptor: &'static ConfigKeyDescriptor,
    key: &str,
    value: &JsonValue,
) -> JsonValue {
    let mut row = base_row(key, Some(descriptor), value);
    // One file has one layer: it either defines the key, or the row shows the
    // built-in value that would apply if this file were the only one.
    let (layer, state) = if store.is_key_set(key) {
        (store.scope().label(), ConfigValueState::Set)
    } else if value.is_null() {
        (
            ConfigValueSourceKind::BuiltIn.label(),
            ConfigValueState::Unset,
        )
    } else {
        (
            ConfigValueSourceKind::BuiltIn.label(),
            ConfigValueState::Default,
        )
    };
    row.insert("state".to_string(), json!(state.label()));
    row.insert(
        "source".to_string(),
        json!({
            "layer": layer,
            "path": store.is_key_set(key).then(|| path_cell(store.path())),
        }),
    );
    row.insert("shadowed_by".to_string(), JsonValue::Array(Vec::new()));
    JsonValue::Object(row)
}

fn base_row(
    key: &str,
    descriptor: Option<&'static ConfigKeyDescriptor>,
    value: &JsonValue,
) -> Map<String, JsonValue> {
    let section = descriptor.map(|descriptor| descriptor.section);
    let mut row = Map::new();
    row.insert("key".to_string(), json!(key));
    row.insert("label".to_string(), json!(label_for(section, key)));
    row.insert("value".to_string(), value.clone());
    row.insert(
        "value_type".to_string(),
        json!(descriptor.map(|descriptor| descriptor.value_type)),
    );
    row.insert("options".to_string(), json!(config_key_options(key)));
    row.insert(
        "section".to_string(),
        json!(section.map(ConfigSection::token)),
    );
    row.insert(
        "description".to_string(),
        json!(descriptor.map(|descriptor| descriptor.description)),
    );
    row
}

/// One row per crew, folded from the per-field `crews.<name>.<field>` values.
fn crew_rows(values: &[EffectiveConfigValue]) -> Vec<JsonValue> {
    let referenced = |key: &str| {
        values
            .iter()
            .find(|entry| entry.key == key)
            .and_then(|entry| entry.value.as_str().map(str::to_string))
    };
    let references = CREW_REFERENCE_KEYS
        .iter()
        .filter_map(|key| referenced(key).map(|crew| (*key, crew)))
        .collect::<Vec<_>>();

    let mut crews: BTreeMap<String, BTreeMap<String, (JsonValue, ConfigValueSourceKind)>> =
        BTreeMap::new();
    for entry in values {
        let Some(rest) = entry.key.strip_prefix("crews.") else {
            continue;
        };
        let Some((name, field)) = rest.split_once('.') else {
            continue;
        };
        crews.entry(name.to_string()).or_default().insert(
            field.to_string(),
            (entry.value.clone(), entry.source.kind()),
        );
    }

    crews
        .into_iter()
        .map(|(name, fields)| {
            let cell = |field: &str| {
                fields
                    .get(field)
                    .map(|(value, _)| value.clone())
                    .unwrap_or(JsonValue::Null)
            };
            // Every crew has built-in projections for the fields it omits, so
            // only the layers that actually define a field are provenance.
            let mut layers = fields
                .values()
                .map(|(_, kind)| kind.label())
                .filter(|label| *label != ConfigValueSourceKind::BuiltIn.label())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if layers.is_empty() {
                layers.push(ConfigValueSourceKind::BuiltIn.label());
            }
            let referenced_by = references
                .iter()
                .filter(|(_, crew)| crew == &name)
                .map(|(key, _)| json!(key))
                .collect::<Vec<_>>();
            json!({
                "name": name,
                "provider": cell("provider"),
                "model": cell("model"),
                "effort": cell("effort"),
                "tags": cell("tags"),
                "description": cell("description"),
                "source": layers.join("+"),
                "referenced_by": referenced_by,
            })
        })
        .collect()
}

fn shadow_note(layer: ConfigValueSourceKind, value: &JsonValue, reason: ShadowReason) -> String {
    let rendered = render_value(value);
    let layer = layer.label();
    match reason {
        ShadowReason::Overridden => format!("overrides {layer}: {rendered}"),
        ShadowReason::NotInherited => {
            format!("{layer} sets {rendered} — not inherited while a workspace file exists")
        }
        ShadowReason::PresetReset => {
            format!("{layer} sets {rendered} — reset by workspace operation.preset")
        }
    }
}

fn render_value(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Array(items) => items.iter().map(render_value).collect::<Vec<_>>().join(" "),
        other => other.to_string(),
    }
}

/// Registry rows for one section, most relevant first.
fn ordered_keys(section: ConfigSection) -> Vec<&'static ConfigKeyDescriptor> {
    let mut keys = CONFIG_KEY_REGISTRY
        .iter()
        .filter(|descriptor| descriptor.section == section)
        .collect::<Vec<_>>();
    keys.sort_by(|left, right| left.order.cmp(&right.order).then(left.key.cmp(right.key)));
    keys
}

fn label_for(section: Option<ConfigSection>, key: &str) -> String {
    match section.and_then(ConfigSection::key_prefix) {
        Some(prefix) => key
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('.'))
            .unwrap_or(key)
            .to_string(),
        None => key.to_string(),
    }
}

/// The registered base branch and ship mode delivery actually reads, plus
/// whether `workflow.base_branch` agrees with it. Null for an unregistered
/// checkout, which is what makes the strip absent rather than empty.
fn workspace_binding_json(runtime: &OrbitRuntime, values: &[EffectiveConfigValue]) -> JsonValue {
    let Some(binding) = runtime.workspace_runtime_binding() else {
        return JsonValue::Null;
    };
    let workflow_base_branch = values
        .iter()
        .find(|entry| entry.key == "workflow.base_branch")
        .and_then(|entry| entry.value.as_str().map(str::to_string));
    let matches = match (&binding.base_branch, &workflow_base_branch) {
        (Some(registered), Some(configured)) => Some(registered == configured),
        _ => None,
    };
    json!({
        "base_branch": binding.base_branch,
        "ship_mode": binding.ship_mode.as_input_value(),
        "owner_machine_id": binding.owner_machine_id,
        "repo_root": path_cell(&binding.repo_root),
        "source": "workspace-registry",
        "workflow_base_branch": workflow_base_branch,
        "base_branch_matches_workflow": matches,
    })
}

fn path_cell(path: &Path) -> String {
    redact_home_dir(&path.display().to_string())
}
