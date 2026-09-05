//! Antigravity (`agy`) headless stdout adapter. [ORB-11299]
//!
//! Official headless output is either one JSON envelope (`--output-format json`)
//! or NDJSON ending in a `result` event (`--output-format stream-json`). Both
//! terminal objects share the same payload: `status`, `response`, optional
//! `error`, and `usage`. Orbit may read the payload only when `status` is
//! `SUCCESS`. Any other terminal status, or a missing/malformed result, yields
//! empty bytes so the completion contract cannot be satisfied by a failed run.

use serde_json::Value;

/// Return the terminal `result` payload when `agy` completed successfully.
///
/// The returned JSON keeps `response` (and `structured_output` when present)
/// for envelope discovery and `usage` for honest token accounting. Failed or
/// malformed terminal objects normalize to empty bytes.
pub(crate) fn normalize_antigravity_stdout(stdout: &[u8]) -> Vec<u8> {
    let Some(result) = terminal_result(stdout) else {
        return Vec::new();
    };
    if result.get("status").and_then(Value::as_str) != Some("SUCCESS") {
        return Vec::new();
    }
    serde_json::to_vec(&result).unwrap_or_default()
}

fn terminal_result(stdout: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(stdout);
    let mut last_result = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if value.get("event").and_then(Value::as_str) == Some("result") {
            last_result = value.get("result").cloned();
            continue;
        }
        if is_json_envelope(&value) {
            last_result = Some(value);
        }
    }
    last_result.or_else(|| {
        let trimmed = text.trim();
        let value = serde_json::from_str::<Value>(trimmed).ok()?;
        is_json_envelope(&value).then_some(value)
    })
}

fn is_json_envelope(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str).is_some()
        && (value.get("response").is_some() || value.get("error").is_some())
}
