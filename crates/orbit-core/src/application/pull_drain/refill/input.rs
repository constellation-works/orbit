//! The drain job's templated activity input, which renders scalars as strings.

use serde_json::Value;

/// Whether the drain was submitted without a window: `for_seconds` zero, as
/// the job forwards it. A pass given no `for_seconds` at all is a timed one.
pub(super) fn single_pass_input(input: &Value) -> bool {
    match input.get("for_seconds") {
        Some(Value::Number(number)) => number.as_f64() == Some(0.0),
        Some(Value::String(text)) => {
            let text = text.trim();
            text.is_empty() || text.parse::<f64>().is_ok_and(|seconds| seconds == 0.0)
        }
        Some(Value::Null) => true,
        _ => false,
    }
}

/// A boolean templated into activity input, which renders as a string.
pub(super) fn bool_input(input: &Value, key: &str) -> bool {
    match input.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(text)) => text.trim() == "true",
        _ => false,
    }
}

pub(super) fn u64_input(input: &Value, key: &str, default: u64) -> u64 {
    match input.get(key) {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(default),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(default),
        _ => default,
    }
}
