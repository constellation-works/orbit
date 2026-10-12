use std::path::PathBuf;

use orbit_common::OrbitError;
use serde_json::Value;

pub(super) fn required_input_string<'a>(
    input: &'a Value,
    key: &str,
) -> Result<&'a str, OrbitError> {
    input
        .as_object()
        .and_then(|map| map.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing required input.{key}")))
}

// pub(crate) widened for tests/ layout under ORB-00225; test reaches via exposed surface.
pub(crate) fn required_job_run_id<'a>(
    input: &'a Value,
    activity: &str,
) -> Result<&'a str, OrbitError> {
    let Some(map) = input.as_object() else {
        return Err(OrbitError::InvalidInput(format!(
            "{activity} requires input.job_run_id, input.run_id, or input.batch_id"
        )));
    };

    for key in ["job_run_id", "run_id", "batch_id"] {
        if let Some(value) = map
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(value);
        }
    }

    Err(OrbitError::InvalidInput(format!(
        "{activity} requires input.job_run_id, input.run_id, or input.batch_id"
    )))
}

pub(super) fn input_string_field(input: &Value, key: &str) -> Option<String> {
    input
        .as_object()
        .and_then(|map| map.get(key))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn canonicalize_existing_dir(
    raw: &str,
    field_name: &str,
) -> Result<PathBuf, OrbitError> {
    let path = PathBuf::from(raw);
    if !path.exists() {
        return Err(OrbitError::InvalidInput(format!(
            "{field_name} does not exist: {raw}"
        )));
    }
    if !path.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "{field_name} is not a directory: {raw}"
        )));
    }
    path.canonicalize().map_err(|error| {
        OrbitError::InvalidInput(format!(
            "failed to canonicalize {field_name} '{raw}': {error}"
        ))
    })
}
