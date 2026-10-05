use std::fs;
use std::path::Path;

use orbit_common::fs::io::atomic_write_text;
use orbit_core::OrbitError;
use serde_json::{Map as JsonMap, Value as JsonValue};

pub(in crate::command::mcp::setup) fn merge_unique_strings(
    existing: &mut Vec<JsonValue>,
    values: Vec<String>,
) {
    let mut seen = existing
        .iter()
        .filter_map(JsonValue::as_str)
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();

    for value in values {
        if seen.insert(value.clone()) {
            existing.push(JsonValue::String(value));
        }
    }
}

pub(in crate::command::mcp::setup) fn remove_known_strings(
    existing: &mut Vec<JsonValue>,
    values: &[String],
) {
    existing.retain(|value| {
        value
            .as_str()
            .map(|candidate| !values.iter().any(|item| item == candidate))
            .unwrap_or(true)
    });
}

pub(in crate::command::mcp::setup) fn load_json_object(
    path: &Path,
) -> Result<JsonMap<String, JsonValue>, OrbitError> {
    if !path.exists() {
        return Ok(JsonMap::new());
    }

    let raw = fs::read_to_string(path)
        .map_err(|err| OrbitError::Io(format!("failed to read '{}': {err}", path.display())))?;
    if raw.trim().is_empty() {
        return Ok(JsonMap::new());
    }

    let value: JsonValue = serde_json::from_str(&raw).map_err(|err| {
        OrbitError::InvalidInput(format!("invalid JSON '{}': {err}", path.display()))
    })?;
    value.as_object().cloned().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "expected top-level JSON object in '{}'",
            path.display()
        ))
    })
}

pub(in crate::command::mcp::setup) fn write_json_object(
    path: &Path,
    root: &JsonMap<String, JsonValue>,
) -> Result<(), OrbitError> {
    let parent = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!("path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)
        .map_err(|err| OrbitError::Io(format!("failed to create '{}': {err}", parent.display())))?;
    let mut rendered =
        serde_json::to_string_pretty(&JsonValue::Object(root.clone())).map_err(|err| {
            OrbitError::Execution(format!("serialize JSON '{}': {err}", path.display()))
        })?;
    rendered.push('\n');
    fs::write(path, rendered)
        .map_err(|err| OrbitError::Io(format!("failed to write '{}': {err}", path.display())))
}

pub(in crate::command::mcp::setup) fn write_json_object_atomic(
    path: &Path,
    root: &JsonMap<String, JsonValue>,
) -> Result<(), OrbitError> {
    let mut rendered =
        serde_json::to_string_pretty(&JsonValue::Object(root.clone())).map_err(|err| {
            OrbitError::Execution(format!("serialize JSON '{}': {err}", path.display()))
        })?;
    rendered.push('\n');
    atomic_write_text(path, &rendered).map_err(|err| {
        OrbitError::Io(format!(
            "failed to atomically write '{}': {err}",
            path.display()
        ))
    })
}

pub(in crate::command::mcp::setup) fn write_or_remove_json_object(
    path: &Path,
    root: &JsonMap<String, JsonValue>,
) -> Result<(), OrbitError> {
    if root.is_empty() {
        if path.exists() {
            fs::remove_file(path).map_err(|err| {
                OrbitError::Io(format!("failed to remove '{}': {err}", path.display()))
            })?;
        }
        return Ok(());
    }
    write_json_object(path, root)
}

pub(in crate::command::mcp::setup) fn ensure_json_object<'a>(
    root: &'a mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<&'a mut JsonMap<String, JsonValue>, OrbitError> {
    let value = root
        .entry(key.to_string())
        .or_insert_with(|| JsonValue::Object(JsonMap::new()));
    value
        .as_object_mut()
        .ok_or_else(|| OrbitError::InvalidInput(format!("expected '{key}' to be a JSON object")))
}

pub(in crate::command::mcp::setup) fn ensure_json_string_array<'a>(
    root: &'a mut JsonMap<String, JsonValue>,
    key: &str,
) -> Result<&'a mut Vec<JsonValue>, OrbitError> {
    let value = root
        .entry(key.to_string())
        .or_insert_with(|| JsonValue::Array(Vec::new()));
    let array = value
        .as_array_mut()
        .ok_or_else(|| OrbitError::InvalidInput(format!("expected '{key}' to be a JSON array")))?;
    if array.iter().any(|item| !item.is_string()) {
        return Err(OrbitError::InvalidInput(format!(
            "expected '{key}' to contain only strings"
        )));
    }
    Ok(array)
}
