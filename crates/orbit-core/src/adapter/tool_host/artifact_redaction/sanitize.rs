use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::security::redaction::{
    is_high_confidence_single_token_credential, redact_all, redact_home_dir,
    redact_sensitive_env_text,
};
use serde_json::{Map, Value, json};

use super::policy::{NestedArrayPolicy, NestedObjectPolicy, TextMode};
use super::{ArtifactRedactionField, ArtifactRedactionKind, ArtifactRedactionReport};

impl ArtifactRedactionKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Pattern => "pattern",
            Self::HomeDir => "home_dir",
        }
    }
}

impl ArtifactRedactionReport {
    pub(super) fn redactions_applied(&self) -> bool {
        !self.fields.is_empty()
    }

    pub(super) fn push(
        &mut self,
        field_path: String,
        kinds: BTreeSet<ArtifactRedactionKind>,
        classes: BTreeSet<&'static str>,
    ) {
        if !kinds.is_empty() {
            self.fields.push(ArtifactRedactionField {
                field_path,
                kinds,
                classes,
            });
        }
    }

    pub(super) fn response_details(&self) -> Value {
        Value::Array(
            self.fields
                .iter()
                .map(|field| {
                    json!({
                        "field_path": field.field_path,
                        "redaction_kinds": field
                            .kinds
                            .iter()
                            .map(|kind| kind.as_str())
                            .collect::<Vec<_>>(),
                        "redaction_classes": field.classes,
                    })
                })
                .collect(),
        )
    }
}

pub(super) fn sanitize_string_field(
    object: &mut Map<String, Value>,
    key: &str,
    field_path: &str,
    mode: TextMode,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    let Some(Value::String(raw)) = object.get(key) else {
        return Ok(());
    };
    let (sanitized, kinds, classes) = sanitize_string(raw, field_path, mode)?;
    if sanitized != *raw {
        object.insert(key.to_string(), Value::String(sanitized));
        report.push(field_path.to_string(), kinds, classes);
    }
    Ok(())
}

pub(super) fn sanitize_string_array_field(
    object: &mut Map<String, Value>,
    key: &str,
    field_path: &str,
    mode: TextMode,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    match object.get_mut(key) {
        Some(Value::String(raw)) => {
            let (sanitized, kinds, classes) = sanitize_string(raw, field_path, mode)?;
            if sanitized != *raw {
                *raw = sanitized;
                report.push(field_path.to_string(), kinds, classes);
            }
        }
        Some(Value::Array(items)) => {
            for (index, item) in items.iter_mut().enumerate() {
                let Value::String(raw) = item else {
                    continue;
                };
                let item_path = format!("{field_path}[{index}]");
                let (sanitized, kinds, classes) = sanitize_string(raw, &item_path, mode)?;
                if sanitized != *raw {
                    *raw = sanitized;
                    report.push(item_path, kinds, classes);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn sanitize_nested_object_fields(
    object: &mut Map<String, Value>,
    policy: &NestedObjectPolicy,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    let Some(Value::Object(nested)) = object.get_mut(policy.object_key) else {
        return Ok(());
    };
    for field in policy.free_text_fields {
        let field_path = format!("{}.{}", policy.object_key, field);
        sanitize_string_field(nested, field, &field_path, TextMode::Free, report)?;
    }
    for field in policy.free_text_arrays {
        let field_path = format!("{}.{}", policy.object_key, field);
        sanitize_string_array_field(nested, field, &field_path, TextMode::Free, report)?;
    }
    Ok(())
}

pub(super) fn sanitize_nested_string_array_field(
    object: &mut Map<String, Value>,
    policy: &NestedArrayPolicy,
    report: &mut ArtifactRedactionReport,
) -> Result<(), OrbitError> {
    if policy.array_key == "scope" {
        let Some(Value::Object(scope)) = object.get_mut("scope") else {
            return Ok(());
        };
        return sanitize_string_array_field(
            scope,
            policy.field_key,
            &format!("scope.{}", policy.field_key),
            policy.mode,
            report,
        );
    }

    let Some(Value::Array(items)) = object.get_mut(policy.array_key) else {
        return Ok(());
    };
    for (index, item) in items.iter_mut().enumerate() {
        let Value::Object(entry) = item else {
            continue;
        };
        let Some(key) = entry
            .contains_key(policy.field_key)
            .then_some(policy.field_key)
            .or_else(|| {
                policy
                    .field_alias
                    .filter(|alias| entry.contains_key(*alias))
            })
        else {
            continue;
        };
        let Some(Value::String(raw)) = entry.get(key) else {
            continue;
        };
        let field_path = format!("{}[{index}].{key}", policy.array_key);
        let (sanitized, kinds, classes) = sanitize_string(raw, &field_path, policy.mode)?;
        if sanitized != *raw {
            entry.insert(key.to_string(), Value::String(sanitized));
            report.push(field_path, kinds, classes);
        }
    }
    Ok(())
}

fn sanitize_string(
    raw: &str,
    field_path: &str,
    mode: TextMode,
) -> Result<
    (
        String,
        BTreeSet<ArtifactRedactionKind>,
        BTreeSet<&'static str>,
    ),
    OrbitError,
> {
    match mode {
        TextMode::PathOnly => {
            let sanitized = redact_home_dir(raw);
            let mut kinds = BTreeSet::new();
            let mut classes = BTreeSet::new();
            if sanitized != raw {
                kinds.insert(ArtifactRedactionKind::HomeDir);
                classes.insert("home_directory");
            }
            Ok((sanitized, kinds, classes))
        }
        TextMode::Free => {
            if is_high_confidence_single_token_credential(raw) {
                return Err(OrbitError::SensitiveInput {
                    field: field_path.to_string(),
                    reason: "whole-token credentials must not be persisted in Orbit artifacts"
                        .to_string(),
                });
            }
            let env_scrubbed = redact_sensitive_env_text(raw);
            let pattern_scrubbed = redact_all(raw);
            let sanitized = redact_home_dir(&pattern_scrubbed);
            let (kinds, classes) =
                classify_redaction_evidence(raw, &env_scrubbed, &pattern_scrubbed, &sanitized);
            Ok((sanitized, kinds, classes))
        }
    }
}

fn classify_redaction_evidence(
    raw: &str,
    env_scrubbed: &str,
    pattern_scrubbed: &str,
    sanitized: &str,
) -> (BTreeSet<ArtifactRedactionKind>, BTreeSet<&'static str>) {
    let mut kinds = BTreeSet::new();
    let mut classes = BTreeSet::new();
    if env_scrubbed != raw {
        kinds.insert(ArtifactRedactionKind::Env);
        classes.insert("sensitive_environment_value");
    }
    if pattern_scrubbed != env_scrubbed {
        kinds.insert(ArtifactRedactionKind::Pattern);
        classes.extend(pattern_redaction_classes(env_scrubbed, pattern_scrubbed));
    }
    if sanitized != pattern_scrubbed {
        kinds.insert(ArtifactRedactionKind::HomeDir);
        classes.insert("home_directory");
    }
    (kinds, classes)
}

fn pattern_redaction_classes(before: &str, after: &str) -> BTreeSet<&'static str> {
    [
        ("[REDACTED_AUTH]", "authorization"),
        ("[REDACTED_SECRET]", "credential"),
        ("[REDACTED_API_KEY]", "credential"),
        ("[REDACTED_SSH_FINGERPRINT]", "ssh_fingerprint"),
        ("[REDACTED_SSH_KEY_COMMENT]", "ssh_key_comment"),
        ("[REDACTED_SSH_HOST]", "ssh_host"),
    ]
    .into_iter()
    .filter_map(|(marker, class)| {
        (after.matches(marker).count() > before.matches(marker).count()).then_some(class)
    })
    .collect()
}

pub(super) fn sanitize_response_value(
    value: &mut Value,
    path: &str,
    report: &mut ArtifactRedactionReport,
) {
    match value {
        Value::String(raw) => {
            let env_scrubbed = redact_sensitive_env_text(raw);
            let pattern_scrubbed = redact_all(&env_scrubbed);
            let sanitized = redact_home_dir(&pattern_scrubbed);
            let (kinds, classes) = classify_redaction_evidence(
                raw.as_str(),
                &env_scrubbed,
                &pattern_scrubbed,
                &sanitized,
            );
            if sanitized != *raw {
                *raw = sanitized;
                report.push(path.to_string(), kinds, classes);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter_mut().enumerate() {
                sanitize_response_value(value, &format!("{path}[{index}]"), report);
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                sanitize_response_value(value, &format!("{path}.{key}"), report);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}
