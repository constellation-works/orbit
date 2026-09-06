//! Antigravity (`agy`) headless stdout adapter. [ORB-11299]
//!
//! Official headless output is either one JSON envelope (`--output-format json`)
//! or NDJSON ending in a `result` event (`--output-format stream-json`). Both
//! terminal objects share the same payload: `status`, `response`, optional
//! `error`, and `usage`. Orbit may read the payload only when `status` is
//! `SUCCESS`. Any other terminal status, or a missing/malformed result, yields
//! empty bytes so the completion contract cannot be satisfied by a failed run.

use serde_json::Value;

/// Upper bound on a provider `error` string copied into a diagnostic. The CLI
/// runner bounds and redacts again; this keeps the extracted text from ever
/// carrying a prompt or response transcript. [ORB-11337]
const TERMINAL_ERROR_LIMIT_CHARS: usize = 400;

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

/// Extract a bounded Antigravity terminal `error` for a failed run.
///
/// Reads only `status` and `error`. The `response` field is never copied, so a
/// failed terminal cannot leak prompt or completion transcripts into
/// diagnostics. SUCCESS terminals yield `None`.
pub(crate) fn antigravity_terminal_error(stdout: &[u8]) -> Option<String> {
    let result = terminal_result(stdout)?;
    let status = result.get("status").and_then(Value::as_str)?;
    if status == "SUCCESS" {
        return None;
    }
    let error_text = terminal_error_text(result.get("error"));
    let text = if error_text.is_empty() {
        format!("Antigravity terminal status {status}")
    } else {
        error_text
    };
    Some(bound_terminal_error(&text))
}

/// Provider-gated diagnostic for a nonzero CLI exit whose stderr is empty.
pub fn antigravity_terminal_error_diagnostic(provider: &str, stdout: &[u8]) -> Option<String> {
    if provider != "antigravity" && provider != "agy" {
        return None;
    }
    let error = antigravity_terminal_error(stdout)?;
    Some(format!("Antigravity terminal error: {error}"))
}

fn terminal_error_text(error: Option<&Value>) -> String {
    match error {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(Value::Object(object)) => object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        _ => String::new(),
    }
}

fn bound_terminal_error(text: &str) -> String {
    let bounded: String = text.chars().take(TERMINAL_ERROR_LIMIT_CHARS).collect();
    if bounded.chars().count() < text.chars().count() {
        format!("{bounded}…")
    } else {
        bounded
    }
}
