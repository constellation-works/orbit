//! Action-input readers and the shared task-pilot action error.

use std::path::PathBuf;

use orbit_engine::DispatchError;
use serde_json::Value;

use crate::OrbitRuntime;

pub(super) fn requested_workspace_root(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<PathBuf, DispatchError> {
    let runtime_root = runtime.paths().repo_root.canonicalize().map_err(|error| {
        action_failed(action, format!("canonicalize runtime workspace: {error}"))
    })?;
    let requested = input
        .get("workspace_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime_root.clone());
    let requested = if requested.is_absolute() {
        requested
    } else {
        runtime_root.join(requested)
    };
    let requested = requested.canonicalize().map_err(|error| {
        action_failed(
            action,
            format!(
                "canonicalize requested workspace {}: {error}",
                requested.display()
            ),
        )
    })?;
    if requested != runtime_root {
        return Err(action_failed(
            action,
            format!(
                "requested workspace {} does not match active workspace {}",
                requested.display(),
                runtime_root.display()
            ),
        ));
    }
    Ok(runtime_root)
}

pub(super) fn bounded_usize(
    action: &str,
    input: &Value,
    field: &str,
    default: usize,
    max: usize,
) -> Result<usize, DispatchError> {
    let value = input
        .get(field)
        .and_then(Value::as_u64)
        .unwrap_or(default as u64);
    if value == 0 || value > max as u64 {
        return Err(action_failed(
            action,
            format!("`{field}` must be between 1 and {max}"),
        ));
    }
    Ok(value as usize)
}

pub(super) fn string_array(
    input: &Value,
    field: &str,
    action: &str,
) -> Result<Vec<String>, DispatchError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(value) => string_array_value(value, field, action),
    }
}

pub(super) fn string_array_value(
    value: &Value,
    field: &str,
    action: &str,
) -> Result<Vec<String>, DispatchError> {
    value
        .as_array()
        .ok_or_else(|| action_failed(action, format!("`{field}` must be an array")))
        .and_then(|values| {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        action_failed(action, format!("`{field}` must contain only strings"))
                    })
                })
                .collect()
        })
}

pub(super) fn required_string_array(
    input: &Value,
    field: &str,
    action: &str,
) -> Result<Vec<String>, DispatchError> {
    input
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, format!("`{field}` must be an array")))
        .and_then(|values| {
            values
                .iter()
                .map(|value| {
                    value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                        action_failed(action, format!("`{field}` must contain only strings"))
                    })
                })
                .collect()
        })
}

pub(super) fn required_string<'a>(
    input: &'a Value,
    field: &str,
    action: &str,
) -> Result<&'a str, DispatchError> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| action_failed(action, format!("`{field}` must be a non-empty string")))
}

pub(super) fn action_failed(action: &str, message: impl Into<String>) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: message.into(),
    }
}
