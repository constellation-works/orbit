use std::collections::{BTreeMap, BTreeSet};

use orbit_config::{
    CONFIG_KEY_REGISTRY, ConfigKeyDescriptor, ConfigScope, ConfigSection, ConfigSnapshot,
    ConfigStore, ConfigValueSourceKind, ConfigValueState, EffectiveConfigValue, ShadowReason,
    admit_settable_config_key, config_key_options, describe_config_key, immutable_key_note,
    is_global_only_key,
};
use serde_json::{Map, Value as JsonValue, json};

use crate::runtime::OrbitRuntime;

use super::paths::path_cell;

/// Fields a crew table carries, in the order a crew row renders them.
pub(super) const CREW_FIELDS: &[&str] = &[
    "enabled",
    "provider",
    "model",
    "effort",
    "tags",
    "description",
];

/// `workflow.*` keys that name a crew by name. Deleting the crew they point at
/// leaves the workspace unable to resolve work, so the delete is refused with
/// the key that still names it.
pub(super) const CREW_REFERENCE_KEYS: &[&str] = &["workflow.default_crew"];

/// Weighted or bare crew lists whose membership the settings view reports.
pub(super) const CREW_POOL_KEYS: &[&str] = &[
    "operation.review_crew",
    "workflow.final_recovery_crews",
    "workflow.low_complexity_crews",
    "workflow.medium_complexity_crews",
    "workflow.hard_complexity_crews",
    "workflow.xhard_complexity_crews",
    "workflow.purpose_crews.task_pilot",
    "workflow.purpose_crews.failure_recovery",
    "workflow.purpose_crews.auto_fix",
    "workflow.purpose_crews.baseline_red_repair",
    "workflow.purpose_crews.ci_failure_sweep",
    "workflow.purpose_crews.merged_head_repair",
    "workflow.purpose_crews.dependabot",
];

/// Keys whose global value the workspace file did not inherit, so the caller
/// can badge the section that owns them.
pub(super) fn not_inherited_keys(values: &[EffectiveConfigValue]) -> JsonValue {
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

pub(super) fn section_catalog() -> JsonValue {
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

pub(super) fn effective_sections(values: &[EffectiveConfigValue]) -> JsonValue {
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

pub(super) fn file_sections(
    context: &FileRowContext<'_>,
    settings: &[(&'static str, JsonValue)],
) -> JsonValue {
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
                                .map(|(key, value)| file_row(context, descriptor, key, value))
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

pub(super) fn effective_row(entry: &EffectiveConfigValue) -> JsonValue {
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
    row.insert(
        "built_in".to_string(),
        super::provenance::built_in_json(&entry.built_in),
    );
    row.insert(
        "fallback".to_string(),
        super::provenance::fallback_json(entry.fallback.as_ref()),
    );
    row.insert(
        "equals".to_string(),
        super::provenance::equals_json(
            &entry.value,
            super::provenance::global_value(entry),
            &entry.built_in,
        ),
    );
    JsonValue::Object(row)
}

/// What every row of one file view is read against.
pub(super) struct FileRowContext<'a> {
    pub(super) store: &'a ConfigStore,
    /// The layer this file is in the layered config: the workspace view of a
    /// checkout without its own root is the global file.
    pub(super) layer: ConfigValueSourceKind,
    /// The layered values, or `None` when the layers do not load.
    pub(super) layered: Option<&'a [EffectiveConfigValue]>,
    pub(super) built_in: ConfigSnapshot,
}

fn file_row(
    context: &FileRowContext<'_>,
    descriptor: &'static ConfigKeyDescriptor,
    key: &str,
    value: &JsonValue,
) -> JsonValue {
    let store = context.store;
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

    let built_in = context.built_in.value_for(key).unwrap_or(JsonValue::Null);
    row.insert(
        "built_in".to_string(),
        super::provenance::built_in_json(&built_in),
    );
    let entry = context
        .layered
        .and_then(|values| values.iter().find(|entry| entry.key == key));
    // Only a value this file sets can be removed or compared.
    let (fallback, equals, reach) = match entry {
        _ if state != ConfigValueState::Set => (JsonValue::Null, JsonValue::Null, JsonValue::Null),
        None => (
            JsonValue::Null,
            super::provenance::equals_json(value, None, &built_in),
            JsonValue::Null,
        ),
        Some(entry) => {
            // The layered fallback is this file's only while this file is
            // the layer in force; a global value the workspace overrides is
            // described by its reach instead.
            let fallback = (entry.source.kind() == context.layer)
                .then_some(entry.fallback.as_ref())
                .flatten();
            let global = (context.layer == ConfigValueSourceKind::Workspace)
                .then(|| super::provenance::global_value(entry))
                .flatten();
            let reach = if store.scope() == ConfigScope::Global {
                super::provenance::reach_json(entry)
            } else {
                JsonValue::Null
            };
            (
                super::provenance::fallback_json(fallback),
                super::provenance::equals_json(value, global, &built_in),
                reach,
            )
        }
    };
    row.insert("fallback".to_string(), fallback);
    row.insert("equals".to_string(), equals);
    row.insert("reach".to_string(), reach);
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
    row.insert(
        "settable".to_string(),
        json!(admit_settable_config_key(key).is_ok()),
    );
    // Why `orbit config set` refuses the key, and whether only the global file
    // may carry it, so a view can explain a refusal before attempting a write.
    row.insert(
        "read_only_reason".to_string(),
        json!(immutable_key_note(key)),
    );
    row.insert("global_only".to_string(), json!(is_global_only_key(key)));
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
///
/// The "Used by" column is decoration: a failed auto-task listing (every
/// definition malformed) omits auto-task references rather than failing the
/// Settings view or a crew write that already committed.
///
/// [ORB-14698] `limit` is the crew's tightest live usage window from the
/// host's provider-limit view, `null` when no reading covers it.
pub(super) fn crew_rows(runtime: &OrbitRuntime, values: &[EffectiveConfigValue]) -> Vec<JsonValue> {
    let limits = runtime.provider_limits_view(chrono::Utc::now());
    let mut references: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in values {
        if CREW_REFERENCE_KEYS.contains(&entry.key.as_str()) {
            if let Some(name) = entry.value.as_str() {
                references
                    .entry(name.trim().to_string())
                    .or_default()
                    .insert(entry.key.clone());
            }
        } else if CREW_POOL_KEYS.contains(&entry.key.as_str())
            && let Some(pool) = entry.value.as_array()
        {
            for member in pool.iter().filter_map(JsonValue::as_str) {
                // Effective config has already admitted the name[:weight] grammar.
                let name = member.split_once(':').map_or(member, |(name, _)| name);
                references
                    .entry(name.trim().to_string())
                    .or_default()
                    .insert(entry.key.clone());
            }
        }
    }
    // Use the scheduler's listing to omit definitions parked by an inactive plugin.
    match runtime.auto_task_listing(false) {
        Ok(listed_tasks) => {
            for listed in listed_tasks {
                let definition = listed.definition;
                if definition.enabled
                    && let Some(name) = definition.template.crew
                {
                    references
                        .entry(name.trim().to_string())
                        .or_default()
                        .insert(format!("auto-task {}", definition.name));
                }
            }
        }
        Err(error) => tracing::warn!(
            %error,
            "auto-task listing unavailable; crew 'Used by' omits auto-task references"
        ),
    }

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
            let referenced_by = references.remove(&name).unwrap_or_default();
            json!({
                "name": name,
                "enabled": cell("enabled"),
                "provider": cell("provider"),
                "model": cell("model"),
                "effort": cell("effort"),
                "tags": cell("tags"),
                "description": cell("description"),
                "source": layers.join("+"),
                "referenced_by": referenced_by,
                "limit": limits.crew(&name),
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
pub(super) fn workspace_binding_json(
    runtime: &OrbitRuntime,
    values: &[EffectiveConfigValue],
) -> JsonValue {
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
