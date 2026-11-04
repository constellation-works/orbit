//! Antigravity (`agy`) headless stdout adapter. [ORB-11299]
//!
//! Official headless output is either one JSON envelope (`--output-format json`)
//! or NDJSON ending in a `result` event (`--output-format stream-json`). Both
//! terminal objects share the same payload: `status`, `response`, optional
//! `error`, and `usage`. Orbit may read the payload only when `status` is
//! `SUCCESS`. Any other terminal status, or a missing/malformed result, yields
//! empty bytes so the completion contract cannot be satisfied by a failed run.

use std::collections::HashMap;

use orbit_types::telemetry::ToolCallTrace;
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

/// The `response` text of the terminal `result`, whatever its status: what the
/// agent last said, without the wrapper's usage and conversation fields.
/// Unbounded and unredacted; the caller bounds it. [ORB-15243]
pub(crate) fn antigravity_terminal_response(stdout: &[u8]) -> Option<String> {
    let result = terminal_result(stdout)?;
    let response = result.get("response").and_then(Value::as_str)?.trim();
    (!response.is_empty()).then(|| response.to_string())
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

/// Tool calls on an Antigravity (`agy`) or legacy Gemini CLI stream.
///
/// `agy --output-format stream-json` reports each tool step twice as
/// `step_update` frames (`step_type: "tool"`, `tool_name`, an `ACTIVE` frame
/// then a `DONE` frame whose `tool_info.output` summarizes the result), and
/// older builds used `tool_call` / `tool_result` events. Gemini CLI
/// `stream-json` uses `type: "tool_use"` / `type: "tool_result"` frames.
/// [`normalize_antigravity_stdout`] keeps only the terminal `result`
/// document, which carries none of these, so the calls are read here from
/// the raw stream.
///
/// `None` when the capture holds no recognized stream frame, so a single
/// `--output-format json` document keeps whatever the generic parser
/// recorded (for Gemini's `-o json` document, no per-call events exist).
/// A recognized stream with no tool frames yields an empty list: an
/// authoritative zero, not a fabricated call.
pub(crate) fn recorded_antigravity_tool_calls(stdout: &[u8]) -> Option<Vec<ToolCallTrace>> {
    let text = String::from_utf8_lossy(stdout);
    let mut calls: Vec<ToolCallTrace> = Vec::new();
    let mut by_key: HashMap<String, usize> = HashMap::new();
    let mut recognized = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(object) = value.as_object() else {
            continue;
        };
        if let Some(event) = object.get("event").and_then(Value::as_str) {
            match event {
                "init" | "step" | "step_update" | "progress" | "result" => recognized = true,
                "tool_call" => {
                    recognized = true;
                    let name = antigravity_tool_name(object);
                    if name.is_empty() {
                        continue;
                    }
                    record_call(
                        &mut calls,
                        &mut by_key,
                        antigravity_tool_id(object).map(str::to_string),
                        name,
                        0,
                    );
                }
                "tool_result" => {
                    recognized = true;
                    let bytes = antigravity_result_bytes(object);
                    let id = antigravity_tool_id(object);
                    if let Some(&index) = id.and_then(|id| by_key.get(id)) {
                        calls[index].result_bytes = bytes;
                    } else if !antigravity_tool_name(object).is_empty() {
                        record_call(
                            &mut calls,
                            &mut by_key,
                            id.map(str::to_string),
                            antigravity_tool_name(object),
                            bytes,
                        );
                    } else if let Some(last) = calls.last_mut() {
                        last.result_bytes = bytes;
                    }
                }
                _ => {}
            }
            if event == "step_update" {
                collect_step_update_tool(object, &mut calls, &mut by_key);
            }
            continue;
        }
        match object.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                recognized = true;
                let name = antigravity_tool_name(object);
                if name.is_empty() {
                    continue;
                }
                record_call(
                    &mut calls,
                    &mut by_key,
                    antigravity_tool_id(object).map(str::to_string),
                    name,
                    0,
                );
            }
            Some("tool_result") => {
                recognized = true;
                let bytes = antigravity_result_bytes(object);
                let id = antigravity_tool_id(object);
                if let Some(&index) = id.and_then(|id| by_key.get(id)) {
                    calls[index].result_bytes = bytes;
                } else if let Some(last) = calls.last_mut() {
                    last.result_bytes = bytes;
                }
            }
            _ => {}
        }
    }
    recognized.then_some(calls)
}

fn record_call(
    calls: &mut Vec<ToolCallTrace>,
    by_key: &mut HashMap<String, usize>,
    key: Option<String>,
    name: &str,
    result_bytes: u64,
) -> usize {
    if let Some(key) = &key
        && let Some(&index) = by_key.get(key)
    {
        if !name.is_empty() {
            calls[index].tool_name = name.to_string();
        }
        if result_bytes > 0 {
            calls[index].result_bytes = result_bytes;
        }
        return index;
    }
    let index = calls.len();
    calls.push(ToolCallTrace {
        seq: (index + 1) as u32,
        tool_name: name.to_string(),
        result_bytes,
        result_payload: None,
    });
    if let Some(key) = key {
        by_key.insert(key, index);
    }
    index
}

fn collect_step_update_tool(
    object: &serde_json::Map<String, Value>,
    calls: &mut Vec<ToolCallTrace>,
    by_key: &mut HashMap<String, usize>,
) {
    let Some(step) = object.get("step_update").and_then(Value::as_object) else {
        return;
    };
    if step.get("step_type").and_then(Value::as_str) != Some("tool") {
        return;
    }
    let name = step
        .get("tool_name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| {
            step.get("tool_info")
                .and_then(|info| info.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    if name.is_empty() {
        return;
    }
    let key = step
        .get("step_index")
        .map(|index| format!("step:{index}"))
        .or_else(|| antigravity_tool_id(step).map(|id| id.to_string()));
    let bytes = step
        .get("tool_info")
        .and_then(|info| info.get("output"))
        .map(serialized_size)
        .unwrap_or(0);
    record_call(calls, by_key, key, name, bytes);
}

fn antigravity_tool_name(object: &serde_json::Map<String, Value>) -> &str {
    object
        .get("tool_name")
        .and_then(Value::as_str)
        .or_else(|| object.get("tool").and_then(Value::as_str))
        .or_else(|| object.get("name").and_then(Value::as_str))
        .unwrap_or_default()
}

fn antigravity_tool_id(object: &serde_json::Map<String, Value>) -> Option<&str> {
    object
        .get("toolCallId")
        .and_then(Value::as_str)
        .or_else(|| object.get("tool_id").and_then(Value::as_str))
        .or_else(|| object.get("tool_use_id").and_then(Value::as_str))
        .or_else(|| object.get("call_id").and_then(Value::as_str))
        .or_else(|| object.get("id").and_then(Value::as_str))
}

fn antigravity_result_bytes(object: &serde_json::Map<String, Value>) -> u64 {
    object
        .get("output")
        .or_else(|| object.get("result"))
        .or_else(|| object.get("content"))
        .map(serialized_size)
        .unwrap_or(0)
}

fn serialized_size(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
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
