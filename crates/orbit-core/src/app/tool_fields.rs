//! Shared field conversions for commands received through the tool entry.

use orbit_common::OrbitError;
use orbit_types::task::TaskPriority;
use serde_json::Value;
use std::str::FromStr;

pub(crate) fn parse_task_priority(field: &str, raw: &str) -> Result<TaskPriority, OrbitError> {
    TaskPriority::from_str(raw)
        .map_err(|error| OrbitError::InvalidInput(format!("`{field}` {error}")))
}

pub(crate) fn optional_u32(input: &Value, field: &str) -> Result<Option<u32>, OrbitError> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!("`{field}` must be a non-negative integer"))
            }),
    }
}

pub(crate) fn serialize_error(label: &'static str) -> impl FnOnce(serde_json::Error) -> OrbitError {
    move |error| OrbitError::Execution(format!("{label}: {error}"))
}
