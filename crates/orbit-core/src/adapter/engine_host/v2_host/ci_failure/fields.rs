//! Snapshot field readers shared across CI failure filing.

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
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[... truncated at {max_bytes} B for the task description; the full excerpt is in \
         the sweep run's step output ...]",
        &value[..end]
    )
}
