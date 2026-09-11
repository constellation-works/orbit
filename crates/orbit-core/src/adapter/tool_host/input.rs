use std::path::PathBuf;
use std::str::FromStr;

use orbit_common::protocol::tool_input::{
    optional_string, optional_string_alias, optional_u32_alias,
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::state_io;
use orbit_tools::OrbitTaskScope;
use orbit_types::task::{
    DEFAULT_TASK_LIST_LIMIT, TaskArtifact, TaskComplexity, TaskPriority, TaskRelation, TaskStatus,
    TaskType, media_type_for_artifact_path,
};
use serde_json::{Value, json};

pub(super) fn resolve_state_dir(
    scope: &OrbitTaskScope,
    input: &Value,
) -> Result<PathBuf, OrbitError> {
    let orbit_root = scope
        .orbit_root
        .clone()
        .or_else(|| std::env::var("ORBIT_ROOT").ok().map(PathBuf::from));
    let orbit_root = orbit_root.ok_or_else(|| {
        OrbitError::InvalidInput(
            "cannot resolve active run path without orbit_root in the trusted runtime scope"
                .to_string(),
        )
    })?;
    // L-0037: run-scoped state tools must trust runtime scope before tool input.
    let scoped_run_id = scope
        .run_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(state_dir) = optional_string_alias(input, &["state_dir", "stateDir", "state-dir"])?
    {
        let run_id = scoped_run_id.ok_or_else(|| {
            OrbitError::InvalidInput(
                "`state_dir` requires an active run in the trusted runtime scope".to_string(),
            )
        })?;
        return state_io::validate_active_run_state_dir(
            &orbit_root,
            &PathBuf::from(state_dir),
            run_id,
        );
    }

    let input_run_id = optional_string_alias(input, &["run_id", "runId", "run-id"])?;
    let run_id = match (scoped_run_id, input_run_id.as_deref()) {
        (Some(scoped), Some(input)) if scoped != input => {
            return Err(OrbitError::InvalidInput(
                "`run_id` must match the active run in the trusted runtime scope".to_string(),
            ));
        }
        (Some(scoped), _) => scoped.to_string(),
        (None, Some(input)) => input.to_string(),
        (None, None) => {
            return Err(OrbitError::InvalidInput(
                "missing active run; provide `run_id` or call from a managed run context"
                    .to_string(),
            ));
        }
    };

    state_io::resolve_active_run_state_dir(&orbit_root, &run_id)?
        .ok_or(OrbitError::not_found(NotFoundKind::JobRun, run_id))
}

/// Resolve the `limit` for a task listing: the caller-supplied value when
/// present (accepting a JSON number or numeric string), otherwise the
/// status-neutral default. A zero limit is rejected with a clear input error
/// (ORB-10310).
pub(super) fn task_list_limit(input: &Value) -> Result<usize, OrbitError> {
    let Some(value) = input.get("limit") else {
        return Ok(DEFAULT_TASK_LIST_LIMIT);
    };
    let limit = match value {
        Value::Null => return Ok(DEFAULT_TASK_LIST_LIMIT),
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| {
                OrbitError::InvalidInput("`limit` must be a positive integer".to_string())
            })
            .and_then(|value| {
                usize::try_from(value)
                    .map_err(|_| OrbitError::InvalidInput("`limit` is too large".to_string()))
            })?,
        Value::String(raw) => raw.trim().parse::<usize>().map_err(|error| {
            OrbitError::InvalidInput(format!("`limit` must be a positive integer: {error}"))
        })?,
        _ => {
            return Err(OrbitError::InvalidInput(
                "`limit` must be a positive integer".to_string(),
            ));
        }
    };
    if limit == 0 {
        return Err(OrbitError::InvalidInput(
            "`limit` must be at least 1".to_string(),
        ));
    }
    Ok(limit)
}

pub(super) fn resolve_step_index(input: &Value) -> Result<u32, OrbitError> {
    if let Some(step_index) = optional_u32_alias(input, &["step_index", "stepIndex", "step-index"])?
    {
        return Ok(step_index);
    }
    let raw = std::env::var("ORBIT_STEP_INDEX").map_err(|_| {
        OrbitError::InvalidInput(
            "missing `step_index`; provide `step_index` or set ORBIT_STEP_INDEX".to_string(),
        )
    })?;
    raw.trim().parse::<u32>().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "ORBIT_STEP_INDEX must be an unsigned integer: {error}"
        ))
    })
}

pub(super) fn resolve_state_payload(input: &Value) -> Result<Value, OrbitError> {
    let data = input.get("data");
    let key = optional_string(input, "key")?;
    let value = input.get("value");
    match (data, key, value) {
        (Some(_), Some(_), _) => Err(OrbitError::InvalidInput(
            "provide either `data` or `key`/`value`, not both".to_string(),
        )),
        (Some(data), None, None) => {
            if !data.is_object() {
                return Err(OrbitError::InvalidInput(
                    "`data` must be a JSON object".to_string(),
                ));
            }
            Ok(data.clone())
        }
        (None, Some(key), Some(value)) => Ok(json!({ key: value.clone() })),
        (None, Some(_), None) => Err(OrbitError::InvalidInput(
            "`value` is required when `key` is provided".to_string(),
        )),
        (None, None, Some(_)) => Err(OrbitError::InvalidInput(
            "`key` is required when `value` is provided".to_string(),
        )),
        (None, None, None) => Err(OrbitError::InvalidInput(
            "provide either `data` or `key`/`value`".to_string(),
        )),
        (Some(_), None, Some(_)) => Err(OrbitError::InvalidInput(
            "provide either `data` or `key`/`value`, not both".to_string(),
        )),
    }
}

pub(super) fn parse_artifacts(input: &Value) -> Result<Vec<TaskArtifact>, OrbitError> {
    let Some(value) = input.get("artifacts").or_else(|| input.get("artifact")) else {
        return Ok(Vec::new());
    };

    match value {
        Value::Null => Ok(Vec::new()),
        Value::Object(map) => map
            .iter()
            .map(|(path, content)| {
                let path = path.trim();
                if path.is_empty() {
                    return Err(OrbitError::InvalidInput(
                        "`artifacts` keys must not be empty".to_string(),
                    ));
                }
                let content = content.as_str().ok_or_else(|| {
                    OrbitError::InvalidInput("`artifacts` values must be strings".to_string())
                })?;
                Ok(TaskArtifact::from_text(path, content))
            })
            .collect(),
        Value::Array(items) => items.iter().map(parse_artifact_array_entry).collect(),
        _ => Err(OrbitError::InvalidInput(
            "`artifacts` must be an object or array".to_string(),
        )),
    }
}

fn parse_artifact_array_entry(item: &Value) -> Result<TaskArtifact, OrbitError> {
    let path = item.get("path").and_then(Value::as_str).ok_or_else(|| {
        OrbitError::InvalidInput(
            "`artifacts` entries must include string `path` values".to_string(),
        )
    })?;
    let path = path.trim();
    if path.is_empty() {
        return Err(OrbitError::InvalidInput(
            "`artifacts` entry paths must not be empty".to_string(),
        ));
    }

    let content_value = item.get("content").ok_or_else(|| {
        OrbitError::InvalidInput("`artifacts` entries must include `content` values".to_string())
    })?;
    let content = parse_artifact_content(content_value)?;
    let media_type = item
        .get("media_type")
        .or_else(|| item.get("mediaType"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| media_type_for_artifact_path(path).to_string());

    Ok(TaskArtifact {
        path: path.to_string(),
        content,
        media_type,
        created_by: None,
    })
}

fn parse_artifact_content(value: &Value) -> Result<Vec<u8>, OrbitError> {
    match value {
        Value::String(content) => Ok(content.as_bytes().to_vec()),
        Value::Array(bytes) => bytes
            .iter()
            .map(|byte| {
                let value = byte.as_u64().ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "`artifacts` byte content must contain unsigned integers".to_string(),
                    )
                })?;
                u8::try_from(value).map_err(|_| {
                    OrbitError::InvalidInput(
                        "`artifacts` byte content values must be between 0 and 255".to_string(),
                    )
                })
            })
            .collect(),
        _ => Err(OrbitError::InvalidInput(
            "`artifacts` entries must include string or byte-array `content` values".to_string(),
        )),
    }
}

pub(super) fn parse_relations(input: &Value) -> Result<Option<Vec<TaskRelation>>, OrbitError> {
    let Some(value) = input.get("relations").or_else(|| input.get("relation")) else {
        return Ok(None);
    };

    match value {
        Value::Null => Ok(Some(Vec::new())),
        Value::Array(_) => serde_json::from_value::<Vec<TaskRelation>>(value.clone())
            .map(Some)
            .map_err(|error| OrbitError::InvalidInput(format!("invalid `relations`: {error}"))),
        Value::Object(_) => serde_json::from_value::<TaskRelation>(value.clone())
            .map(|relation| Some(vec![relation]))
            .map_err(|error| OrbitError::InvalidInput(format!("invalid `relations`: {error}"))),
        _ => Err(OrbitError::InvalidInput(
            "`relations` must be an array of {type, target} objects".to_string(),
        )),
    }
}

pub(super) fn empty_string_to_none(raw: String) -> Option<String> {
    if raw.trim().is_empty() {
        None
    } else {
        Some(raw)
    }
}

pub(super) fn optional_bool_alias(
    input: &Value,
    names: &[&str],
) -> Result<Option<bool>, OrbitError> {
    for name in names {
        let Some(value) = input.get(*name) else {
            continue;
        };
        return match value {
            Value::Bool(value) => Ok(Some(*value)),
            Value::String(raw) => match raw.trim().to_ascii_lowercase().as_str() {
                "true" => Ok(Some(true)),
                "false" => Ok(Some(false)),
                _ => Err(OrbitError::InvalidInput(format!(
                    "`{name}` must be a boolean"
                ))),
            },
            _ => Err(OrbitError::InvalidInput(format!(
                "`{name}` must be a boolean"
            ))),
        };
    }
    Ok(None)
}

pub(super) fn parse_task_status(field: &str, raw: &str) -> Result<TaskStatus, OrbitError> {
    TaskStatus::from_str(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("`{field}` {error}")))
}

pub(super) fn parse_task_priority(field: &str, raw: &str) -> Result<TaskPriority, OrbitError> {
    TaskPriority::from_str(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("`{field}` {error}")))
}

fn parse_task_complexity(field: &str, raw: &str) -> Result<TaskComplexity, OrbitError> {
    TaskComplexity::from_str(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("`{field}` {error}")))
}

/// Parse a complexity that a human or agent may assign on create or update.
///
/// [`TaskComplexity::Unassessed`] is reserved for automated mint/import, so an
/// agent cannot pass the create assessment and then clear it one update later.
pub(super) fn parse_assessed_task_complexity(
    field: &str,
    raw: &str,
) -> Result<TaskComplexity, OrbitError> {
    parse_task_complexity(field, raw)?
        .require_assessed()
        .map_err(OrbitError::InvalidInput)
}

pub(super) fn parse_task_type(field: &str, raw: &str) -> Result<TaskType, OrbitError> {
    TaskType::from_str(raw).map_err(|error| OrbitError::InvalidInput(format!("`{field}` {error}")))
}

pub(super) fn require_object_field<'a>(
    input: &'a Value,
    field: &str,
) -> Result<&'a Value, OrbitError> {
    let value = input
        .get(field)
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{field}`")))?;
    if !value.is_object() {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` must be a JSON object"
        )));
    }
    Ok(value)
}

pub(super) fn parse_string_array_field(
    input: &Value,
    field: &str,
) -> Result<Vec<String>, OrbitError> {
    let value = input
        .get(field)
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{field}`")))?;
    let values = match value {
        Value::String(raw) => vec![raw.as_str()],
        Value::Array(items) => {
            if items.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{field}` must contain at least one value"
                )));
            }
            items
                .iter()
                .map(|item| {
                    item.as_str().ok_or_else(|| {
                        OrbitError::InvalidInput(format!("`{field}` entries must be strings"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        _ => {
            return Err(OrbitError::InvalidInput(format!(
                "`{field}` must be a string or array of strings"
            )));
        }
    };
    values
        .into_iter()
        .map(|raw| {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "`{field}` entries must not be empty"
                )));
            }
            Ok(trimmed.to_string())
        })
        .collect()
}

pub(super) fn parse_optional_string_array_field(
    input: &Value,
    field: &str,
) -> Result<Vec<String>, OrbitError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) if items.is_empty() => Ok(Vec::new()),
        Some(_) => parse_string_array_field(input, field),
    }
}

pub(super) fn parse_optional_poll_interval_seconds(
    input: &Value,
) -> Result<Option<u64>, OrbitError> {
    let Some(value) = input.get("poll_interval_seconds") else {
        return Ok(None);
    };
    let seconds = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) => raw.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        OrbitError::InvalidInput("`poll_interval_seconds` must be a positive number".to_string())
    })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(OrbitError::InvalidInput(
            "`poll_interval_seconds` must be a positive number".to_string(),
        ));
    }
    Ok(Some(seconds.floor().max(1.0) as u64))
}

pub(super) fn parse_optional_timeout_seconds(input: &Value) -> Result<Option<u64>, OrbitError> {
    let Some(value) = input.get("timeout_seconds") else {
        return Ok(None);
    };
    let seconds = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(raw) => raw.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| {
        OrbitError::InvalidInput("`timeout_seconds` must be an unsigned integer".to_string())
    })?;
    Ok(Some(seconds))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parse_string_array_field_accepts_scalar_string() {
        assert_eq!(
            parse_string_array_field(&json!({"run_ids":"run-1"}), "run_ids").unwrap(),
            vec!["run-1"]
        );
    }

    #[test]
    fn parse_string_array_field_preserves_array_behavior() {
        assert_eq!(
            parse_string_array_field(&json!({"run_ids":["run-1", "run-2"]}), "run_ids").unwrap(),
            vec!["run-1", "run-2"]
        );
    }

    #[test]
    fn parse_string_array_field_rejects_non_string_shapes() {
        let error = parse_string_array_field(&json!({"run_ids":{"id":"run-1"}}), "run_ids")
            .unwrap_err()
            .to_string();
        assert!(error.contains("`run_ids` must be a string or array of strings"));
    }

    #[test]
    fn parse_optional_string_array_field_treats_empty_values_as_unrestricted() {
        assert_eq!(
            parse_optional_string_array_field(&json!({}), "allowed_crews").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_optional_string_array_field(&json!({"allowed_crews": null}), "allowed_crews")
                .unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            parse_optional_string_array_field(&json!({"allowed_crews": []}), "allowed_crews")
                .unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parse_artifacts_accepts_text_and_byte_array_content() {
        let artifacts = parse_artifacts(&json!({
            "artifacts": [
                {"path": "notes.txt", "content": "hello"},
                {"path": "image.bin", "media_type": "application/octet-stream", "content": [0, 159, 255]}
            ]
        }))
        .unwrap();

        assert_eq!(artifacts[0].text_content(), Some("hello"));
        assert_eq!(artifacts[0].media_type, "text/plain");
        assert_eq!(artifacts[1].content, vec![0, 159, 255]);
        assert_eq!(artifacts[1].media_type, "application/octet-stream");
    }

    #[test]
    fn parse_artifacts_rejects_invalid_byte_content() {
        let error = parse_artifacts(&json!({
            "artifacts": [{"path": "image.bin", "content": [256]}]
        }))
        .unwrap_err()
        .to_string();

        assert!(error.contains("between 0 and 255"));
    }
}
