//! Snapshot field readers shared across CI failure filing.

use orbit_common::text::floor_char_boundary;
use serde_json::Value;

/// Read a snapshot field as a display string, accepting the numeric spellings
/// `gh` uses for run and job identifiers.
pub(super) fn value_string(value: &Value, key: &str) -> String {
    match value.get(key) {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

pub(super) fn run_order(run: &Value) -> (String, u64) {
    (
        value_string(run, "created_at"),
        run.get("run_id").and_then(Value::as_u64).unwrap_or(0),
    )
}

pub(super) fn truncate_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let end = floor_char_boundary(value, max_bytes);
    format!(
        "{}\n[... truncated at {max_bytes} B for the task description; the full excerpt is in \
         the sweep run's step output ...]",
        &value[..end]
    )
}
