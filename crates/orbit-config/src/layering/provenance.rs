//! Per-value sources, shadowed layers, and fallbacks.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_types::identity::Crew;

use crate::registry::ConfigSnapshot;
use crate::resolved::{ResolvedConfig, crews_from_raw, default_crews};

use super::document::{crew_entry, empty_document, merge_tables};
use super::load::ConfigDocument;
use super::{
    ConfigFallback, ConfigValueSource, ConfigValueSourceKind, DEFAULT_CREW_KEY,
    DEFAULT_PROVIDER_ENV, EffectiveConfigValue, ShadowReason, ShadowedConfigValue,
    WORKSPACE_REPLACE_ONLY_KEYS, value_at_path,
};

pub(super) fn effective_values(
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
                fallback: fallback_for_crew_field(name, field, &source, global, workspace),
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
/// merge the surviving file fields and admit the crew again. Shipped crews
/// are a registry default, not a lower field layer in a configured table.
fn fallback_for_crew_field(
    crew: &str,
    field: &str,
    source: &ConfigValueSource,
    global: Option<&ConfigDocument>,
    workspace: Option<&ConfigDocument>,
) -> Option<ConfigFallback> {
    let path = source.path()?;
    let mut global_entry = global
        .and_then(|document| crew_entry(&document.value, crew))
        .cloned()
        .unwrap_or_default();
    let mut workspace_entry = workspace
        .and_then(|document| crew_entry(&document.value, crew))
        .cloned()
        .unwrap_or_default();
    let layer = match source.kind() {
        ConfigValueSourceKind::Workspace => {
            workspace_entry.remove(field);
            if global_entry.contains_key(field) {
                ConfigValueSourceKind::Global
            } else {
                ConfigValueSourceKind::BuiltIn
            }
        }
        ConfigValueSourceKind::Global => {
            global_entry.remove(field);
            ConfigValueSourceKind::BuiltIn
        }
        _ => return None,
    };
    let mut entry = toml::Value::Table(global_entry);
    merge_tables(&mut entry, &toml::Value::Table(workspace_entry));
    let raw = BTreeMap::from([(crew.to_string(), entry.try_into().ok()?)]);
    // Reuse admission for required fields, optional defaults, normalization,
    // and provider/model-dependent effort. A failed admission is unknown,
    // not a promise that removing the field will reveal a shipped value.
    let (crews, _) = crews_from_raw(Some(&raw), path, &|_, _| path.to_path_buf()).ok()?;
    let value = crew_field_values(crews.get(crew)?)
        .into_iter()
        .find(|(name, _)| *name == field)
        .map_or(serde_json::Value::Null, |(_, value)| value);
    Some(ConfigFallback { layer, value })
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

pub(super) fn source_for_crew_field(
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
