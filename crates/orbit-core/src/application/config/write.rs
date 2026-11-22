use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_config::{
    ConfigScope, ConfigStore, ConfigValueSourceKind, WorkspaceInitMode, admit_config_key,
    load_effective_config,
};
use serde_json::{Map, Value as JsonValue, json};

use crate::runtime::OrbitRuntime;

use super::paths::{config_roots, global_config_path, workspace_config_path};
use super::provenance::fallback_preserves_effective;
use super::rows::{CREW_FIELDS, CREW_REFERENCE_KEYS, crew_rows, effective_row};
use super::view::{crew_value, effective_value};

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
    let literal = toml_literal(value)?;
    set_key_literal(runtime, key, &literal, scope, init)
}

/// Write a raw TOML literal, falling back to a plain string when it does not
/// parse as one. This preserves the local CLI's value syntax while sharing
/// the typed JSON write's admission, transaction, and outcome.
pub fn set_key_literal(
    runtime: &OrbitRuntime,
    key: &str,
    literal: &str,
    scope: ConfigScope,
    init: ConfigWriteInit,
) -> Result<ConfigWriteOutcome, OrbitError> {
    admit_config_key(key)?;
    let (store, old_value) = write_store(runtime, scope, init, |store| {
        let old_value = effective_value(runtime, key)?;
        store.set_value(key, literal)?;
        match scope {
            ConfigScope::Global => {
                store.validate()?;
                store.validate_global_for_set(key, &runtime.shared_root())?;
            }
            ConfigScope::Workspace => {
                let global_root = runtime.global_root();
                store.validate_workspace_for_set(key, &global_root)?;
            }
        }
        Ok(old_value)
    })?;
    write_outcome(runtime, scope, &store, old_value, &[key.to_string()])
}

/// Why a guarded config removal refused to clear a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigUnsetRefusal {
    /// Stable machine-readable refusal code.
    pub code: &'static str,
    /// Human-readable reason explaining why the value was preserved.
    pub message: String,
}

/// Result of a guarded config key removal that requires preserving the
/// effective value.
#[derive(Debug, Clone)]
pub enum GuardedConfigUnset {
    /// The key was cleared without changing the effective value.
    Unset(ConfigWriteOutcome),
    /// The key was preserved because removal would change or cannot predict
    /// the effective value.
    Refused(ConfigUnsetRefusal),
}

/// Clear one key from `scope`'s file so the layer below takes over again.
pub fn unset_key(
    runtime: &OrbitRuntime,
    key: &str,
    scope: ConfigScope,
) -> Result<ConfigWriteOutcome, OrbitError> {
    admit_config_key(key)?;
    let (store, old_value) =
        write_store(runtime, scope, ConfigWriteInit::RequireExisting, |store| {
            let old_value = effective_value(runtime, key)?;
            if !store.unset_value(key)? {
                return Err(OrbitError::InvalidInput(format!(
                    "'{key}' is not set in the {} config; nothing to clear",
                    scope.label()
                )));
            }
            validate_store(runtime, scope, store)?;
            Ok(old_value)
        })?;
    write_outcome(runtime, scope, &store, old_value, &[key.to_string()])
}

/// Clear one key from `scope`'s file only when its admitted current effective
/// value still equals its admitted current removal fallback.
///
/// Re-resolves the layered configuration under the target file's write lock so
/// a concurrent edit to either layer cannot turn bulk cleanup into a silent
/// effective-value change. An unknown (`None`) or mismatched fallback refuses
/// without modifying the file.
pub fn unset_key_preserving_effective(
    runtime: &OrbitRuntime,
    key: &str,
    scope: ConfigScope,
) -> Result<GuardedConfigUnset, OrbitError> {
    admit_config_key(key)?;
    let expected_layer = match scope {
        ConfigScope::Workspace if runtime.shared_root() != runtime.global_root() => {
            ConfigValueSourceKind::Workspace
        }
        _ => ConfigValueSourceKind::Global,
    };
    let result = write_store_conditional(
        runtime,
        scope,
        ConfigWriteInit::RequireExisting,
        |store| {
            if !store.is_key_set(key) {
                return Err(OrbitError::InvalidInput(format!(
                    "'{key}' is not set in the {} config; nothing to clear",
                    scope.label()
                )));
            }
            let effective = load_effective_config(&config_roots(runtime))?;
            let Some(entry) = effective.values().iter().find(|entry| entry.key == key) else {
                return Ok(ConditionalEdit::Abort(ConfigUnsetRefusal {
                    code: "unknown_fallback",
                    message: format!(
                        "removal fallback for '{key}' is unknown; {} value preserved",
                        scope.label()
                    ),
                }));
            };
            if entry.source.kind() != expected_layer {
                return Ok(ConditionalEdit::Abort(ConfigUnsetRefusal {
                    code: "effective_source_changed",
                    message: format!(
                        "'{key}' currently resolves from {} rather than {}; {} value preserved",
                        entry.source.kind().label(),
                        expected_layer.label(),
                        scope.label()
                    ),
                }));
            }
            let Some(fallback) = entry.fallback.as_ref() else {
                return Ok(ConditionalEdit::Abort(ConfigUnsetRefusal {
                    code: "unknown_fallback",
                    message: format!(
                        "removal fallback for '{key}' is unknown; {} value preserved",
                        scope.label()
                    ),
                }));
            };
            if !fallback_preserves_effective(entry, expected_layer) {
                return Ok(ConditionalEdit::Abort(ConfigUnsetRefusal {
                    code: "effective_value_would_change",
                    message: format!(
                        "removing '{key}' from the {} config would change the effective value from {} to {} ({}); value preserved",
                        scope.label(),
                        entry.value,
                        fallback.value,
                        fallback.layer.label()
                    ),
                }));
            }
            let old_value = entry.value.clone();
            if !store.unset_value(key)? {
                return Err(OrbitError::InvalidInput(format!(
                    "'{key}' is not set in the {} config; nothing to clear",
                    scope.label()
                )));
            }
            validate_store(runtime, scope, store)?;
            if scope == ConfigScope::Workspace {
                // The staged document is what a reload will see: the removal
                // can empty an implicit dotted table that serialization
                // omits, leaving a file that no longer layers as policy.
                let staged_value =
                    store.staged_workspace_effective_value(key, &runtime.global_root())?;
                if !store.staged_workspace_sets_policy()
                    || staged_value.is_some_and(|value| value != old_value)
                {
                    return Ok(ConditionalEdit::Abort(ConfigUnsetRefusal {
                        code: "effective_value_would_change",
                        message: format!(
                            "removing '{key}' from the {} config would change the effective configuration; value preserved",
                            scope.label()
                        ),
                    }));
                }
            }
            Ok(ConditionalEdit::Save(old_value))
        },
    )?;
    match result {
        ConditionalWrite::Saved(store, old_value) => Ok(GuardedConfigUnset::Unset(write_outcome(
            runtime,
            scope,
            &store,
            old_value,
            &[key.to_string()],
        )?)),
        ConditionalWrite::Aborted(refusal) => Ok(GuardedConfigUnset::Refused(refusal)),
    }
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
    let (store, old_value) = write_store(runtime, scope, init, |store| {
        let old_value = crew_value(runtime, name)?;
        for (field, value) in fields {
            let key = format!("crews.{name}.{field}");
            if value.is_null() {
                store.unset_value(&key)?;
            } else {
                store.set_value(&key, &toml_literal(value)?)?;
            }
        }
        validate_store(runtime, scope, store)?;
        for key in &keys {
            match scope {
                ConfigScope::Global => {
                    store.validate_global_for_set(key, &runtime.shared_root())?;
                }
                ConfigScope::Workspace => {
                    let global_root = runtime.global_root();
                    store.validate_workspace_for_set(key, &global_root)?;
                }
            }
        }
        Ok(old_value)
    })?;
    write_outcome(runtime, scope, &store, old_value, &keys)
}

/// Delete one crew table, refusing while a `workflow.*` key still names it.
pub fn delete_crew(
    runtime: &OrbitRuntime,
    name: &str,
    scope: ConfigScope,
) -> Result<ConfigWriteOutcome, OrbitError> {
    let (store, old_value) =
        write_store(runtime, scope, ConfigWriteInit::RequireExisting, |store| {
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
            if !store.remove_crew_table(name)? {
                return Err(OrbitError::InvalidInput(format!(
                    "crew '{name}' is not defined in the {} config",
                    scope.label()
                )));
            }
            validate_store(runtime, scope, store)?;
            Ok(old_value)
        })?;
    write_outcome(runtime, scope, &store, old_value, &[])
}

fn validate_store(
    runtime: &OrbitRuntime,
    scope: ConfigScope,
    store: &ConfigStore,
) -> Result<(), OrbitError> {
    match scope {
        ConfigScope::Global => {
            store.validate()?;
            store.validate_global_with_workspace(&runtime.shared_root())
        }
        ConfigScope::Workspace => store.validate_workspace_with_global(&runtime.global_root()),
    }
}

enum ConditionalEdit<T, R> {
    Save(T),
    Abort(R),
}

enum ConditionalWrite<T, R> {
    Saved(Box<ConfigStore>, T),
    Aborted(R),
}

/// One write of `scope`'s file as a transaction: under the file's write lock,
/// open it, let `edit` mutate and validate the staged document, then save
/// exactly what was validated. A concurrent writer of the same file waits for
/// the lock, so neither edit erases the other.
fn write_store<T>(
    runtime: &OrbitRuntime,
    scope: ConfigScope,
    init: ConfigWriteInit,
    edit: impl FnOnce(&mut ConfigStore) -> Result<T, OrbitError>,
) -> Result<(ConfigStore, T), OrbitError> {
    match write_store_conditional(runtime, scope, init, |store| {
        edit(store).map(ConditionalEdit::<T, std::convert::Infallible>::Save)
    })? {
        ConditionalWrite::Saved(store, value) => Ok((*store, value)),
        ConditionalWrite::Aborted(never) => match never {},
    }
}

/// Locked config transaction whose `edit` may refuse before saving, leaving
/// the underlying file untouched.
fn write_store_conditional<T, R>(
    runtime: &OrbitRuntime,
    scope: ConfigScope,
    init: ConfigWriteInit,
    edit: impl FnOnce(&mut ConfigStore) -> Result<ConditionalEdit<T, R>, OrbitError>,
) -> Result<ConditionalWrite<T, R>, OrbitError> {
    if scope == ConfigScope::Workspace {
        let cwd = std::env::current_dir().map_err(|err| OrbitError::Io(err.to_string()))?;
        // `--root` pins global and shared to one directory. `ORBIT_ROOT` is
        // detected inside the refusal. Either one may name the workspace, as
        // may an explicit workspace selector the composition layer resolved.
        let explicit_root = runtime.global_root() == runtime.shared_root()
            || runtime.workspace_explicitly_selected();
        crate::runtime::refuse_indirect_uninitialized_caller_orbit(
            &cwd,
            &runtime.shared_root(),
            explicit_root,
        )?;
    }
    let path = match scope {
        ConfigScope::Global => global_config_path(runtime),
        ConfigScope::Workspace => workspace_config_path(runtime),
    };
    ConfigStore::with_write_lock(&path, || {
        let mut store = match scope {
            ConfigScope::Global => ConfigStore::open(ConfigScope::Global, path.clone())?,
            ConfigScope::Workspace => ConfigStore::open_for_workspace_set(
                path.clone(),
                &global_config_path(runtime),
                init.mode(),
            )?,
        };
        match edit(&mut store)? {
            ConditionalEdit::Save(edited) => {
                store.save()?;
                Ok(ConditionalWrite::Saved(Box::new(store), edited))
            }
            ConditionalEdit::Abort(refusal) => Ok(ConditionalWrite::Aborted(refusal)),
        }
    })
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
            crew_rows(runtime, values)
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
