//! Helpers shared by the sweep actions that file tasks from external evidence
//! (`file_ci_failure_tasks`, `file_dependabot_alert_tasks`).

use orbit_common::OrbitError;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Hex characters of a signature digest kept in a tag. Full-width digests
/// make a tag unreadable in a task list; this is a dedupe key, not a security
/// boundary.
const KEY_LEN: usize = 16;

/// Short stable key over `parts`, NUL-separated so part boundaries matter.
pub(super) fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0u8]);
    }
    format!("{:x}", hasher.finalize())
        .chars()
        .take(KEY_LEN)
        .collect()
}

/// Read optional positive integer `input.<key>` (number or numeric string),
/// defaulting when absent and capping at `max`.
pub(super) fn bounded_u64(
    input: &Value,
    key: &str,
    default: u64,
    max: u64,
) -> Result<u64, OrbitError> {
    let Some(value) = input.get(key).filter(|value| !value.is_null()) else {
        return Ok(default);
    };
    let raw = match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.trim().parse::<u64>().ok(),
        _ => None,
    }
    .ok_or_else(|| OrbitError::InvalidInput(format!("input.{key} must be a positive integer")))?;
    if raw == 0 {
        return Err(OrbitError::InvalidInput(format!(
            "input.{key} must be greater than zero"
        )));
    }
    Ok(raw.min(max))
}

pub(super) fn display(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

pub(super) fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect::<String>() + "…"
}
