//! Per-key comparison facts for the Settings rows: the built-in default, the
//! fallback a removal would reach, whether a value restates the global or
//! built-in one, and how far a global value reaches into the bound workspace.
//!
//! Every fact is read off [`EffectiveConfigValue`]; the layering rules that
//! decide a fallback or a not-inherited value stay in `orbit-config`.
//! Equality is plain JSON equality over the values the config layer reports,
//! with no normalisation of its own.

use orbit_config::{
    ConfigFallback, ConfigValueSourceKind, ConfigValueState, EffectiveConfigValue, ShadowReason,
};
use serde_json::{Value as JsonValue, json};

/// The value Orbit ships with, stated as `unset` rather than an empty value
/// when there is none.
pub(super) fn built_in_json(value: &JsonValue) -> JsonValue {
    json!({
        "state": built_in_state(value).label(),
        "value": value,
    })
}

/// The layer and value the key reaches if its own layer stops setting it, or
/// null for a row with nothing to remove.
pub(super) fn fallback_json(fallback: Option<&ConfigFallback>) -> JsonValue {
    let Some(fallback) = fallback else {
        return JsonValue::Null;
    };
    let state = match fallback.layer {
        ConfigValueSourceKind::BuiltIn => built_in_state(&fallback.value),
        _ => ConfigValueState::Set,
    };
    json!({
        "layer": fallback.layer.label(),
        "value": fallback.value,
        "state": state.label(),
    })
}

/// Whether `value` restates the global value (null when the global file does
/// not set the key) and the built-in default.
pub(super) fn equals_json(
    value: &JsonValue,
    global: Option<&JsonValue>,
    built_in: &JsonValue,
) -> JsonValue {
    json!({
        "global": global.map(|global| global == value),
        "built_in": built_in == value,
    })
}

/// The value the global file sets for this key, whether or not it is the one
/// in force.
pub(super) fn global_value(entry: &EffectiveConfigValue) -> Option<&JsonValue> {
    entry.global_value.as_ref()
}

/// How far a value the global file sets reaches into the bound workspace: in
/// effect, overridden by the workspace file (with its value and whether it
/// equals `global`), or not inherited by a replace-only security key.
pub(super) fn reach_json(entry: &EffectiveConfigValue) -> JsonValue {
    let not_inherited = entry
        .shadowed_by
        .iter()
        .any(|shadow| shadow.reason == ShadowReason::NotInherited);
    match entry.source.kind() {
        ConfigValueSourceKind::Global => json!({"state": "in-effect"}),
        ConfigValueSourceKind::Workspace => json!({
            "state": "overridden",
            "workspace_value": entry.value,
            "equals_global": global_value(entry).map(|global| &entry.value == global),
        }),
        _ if not_inherited => json!({"state": "not-inherited"}),
        // Layering records a shadow whenever the global value is not the one
        // in force, so this is unreachable; null says "unknown", not a guess.
        _ => JsonValue::Null,
    }
}

fn built_in_state(value: &JsonValue) -> ConfigValueState {
    if value.is_null() {
        ConfigValueState::Unset
    } else {
        ConfigValueState::Default
    }
}
