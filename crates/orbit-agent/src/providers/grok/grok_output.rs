//! Projects Grok CLI stdout onto the final answer, and reduces a
//! `streaming-json` capture to one usage document.
//!
//! `grok --output-format json` returns one wrapper object. Its `text` field
//! is the model's final answer; `thought`, tool metadata, and usage are
//! invocation telemetry and must not provide Orbit completion evidence.
//!
//! `grok --output-format streaming-json` is newline-delimited. Answer text
//! is the concatenation of `type: text` chunks. Spend fields that match the
//! json wrapper live on the final `type: end` event; per-response `usage`
//! lines describe the same run and are not summed with it. Tool calls are
//! `type: tool_call` (`toolName`, `toolCallId`) and `type: tool_call_update`
//! (`rawOutput`). A capture that is not this stream stays on the json path.

use std::collections::HashMap;

use orbit_types::telemetry::ToolCallTrace;
use serde_json::{Map, Value};

/// Return the final answer when `stdout` is a recognized Grok capture.
///
/// A json wrapper yields its `text`. A `streaming-json` capture yields the
/// concatenated `text` chunks, including an empty answer, so the raw event
/// stream cannot be read as the envelope. `None` keeps the legacy path for
/// direct Orbit envelopes and any other multi-document output.
pub(crate) fn project_grok_response(stdout: &[u8]) -> Option<Vec<u8>> {
    if let Some(events) = grok_streaming_events(stdout) {
        return Some(grok_answer_text(&events).into_bytes());
    }

    let wrapper = serde_json::from_slice::<Value>(stdout).ok()?;
    let wrapper = wrapper.as_object()?;

    if !wrapper.contains_key("text") && !wrapper.contains_key("stopReason") {
        return None;
    }

    Some(
        wrapper
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
    )
}

/// The json-wrapper document a `streaming-json` capture reduces to, so usage
/// is the `end` event once. `None` leaves the capture unchanged: it is not a
/// recognized stream, or the stream was cut off before `end`.
pub(crate) fn normalize_grok_stdout(stdout: &[u8]) -> Option<Vec<u8>> {
    let events = grok_streaming_events(stdout)?;
    let end = events
        .iter()
        .rev()
        .find(|event| event_type(event) == Some("end"))?;
    let mut wrapper = Map::new();
    wrapper.insert("text".to_string(), Value::String(grok_answer_text(&events)));
    for key in [
        "stopReason",
        "usage",
        "modelUsage",
        "total_cost_usd",
        "num_turns",
    ] {
        if let Some(value) = end.get(key) {
            wrapper.insert(key.to_string(), value.clone());
        }
    }
    serde_json::to_vec(&Value::Object(wrapper)).ok()
}

/// Tool calls on a recognized `streaming-json` capture. `None` when `stdout`
/// is not that stream, so a json wrapper keeps whatever the generic parser
/// already recorded.
pub(crate) fn recorded_grok_tool_calls(stdout: &[u8]) -> Option<Vec<ToolCallTrace>> {
    let events = grok_streaming_events(stdout)?;
    let mut calls: Vec<ToolCallTrace> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for event in &events {
        match event_type(event) {
            Some("tool_call") => {
                let Some(name) = event.get("toolName").and_then(Value::as_str) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                let index = calls.len();
                if let Some(id) = event.get("toolCallId").and_then(Value::as_str) {
                    if let Some(&existing) = by_id.get(id) {
                        calls[existing].tool_name = name.to_string();
                        continue;
                    }
                    by_id.insert(id.to_string(), index);
                }
                calls.push(ToolCallTrace {
                    seq: (index + 1) as u32,
                    tool_name: name.to_string(),
                    result_bytes: 0,
                    result_payload: None,
                });
            }
            Some("tool_call_update") => {
                let Some(id) = event.get("toolCallId").and_then(Value::as_str) else {
                    continue;
                };
                let Some(&index) = by_id.get(id) else {
                    continue;
                };
                if let Some(output) = event.get("rawOutput") {
                    calls[index].result_bytes = serialized_size(output);
                }
            }
            _ => {}
        }
    }
    Some(calls)
}

/// Recognized when more than one JSON object is present and at least one is
/// an `end` event or a `tool_call` that carries `toolName`. A line that is
/// not JSON makes the capture untrusted and returns `None`.
fn grok_streaming_events(stdout: &[u8]) -> Option<Vec<Value>> {
    let mut events = Vec::new();
    let mut recognized = false;
    for line in stdout.split(|byte| *byte == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let value = serde_json::from_slice::<Value>(line).ok()?;
        if !value.is_object() {
            return None;
        }
        recognized |= match event_type(&value) {
            Some("end") => true,
            Some("tool_call") => value.get("toolName").and_then(Value::as_str).is_some(),
            _ => false,
        };
        events.push(value);
    }
    (events.len() > 1 && recognized).then_some(events)
}

fn grok_answer_text(events: &[Value]) -> String {
    let mut text = String::new();
    for event in events {
        if event_type(event) != Some("text") {
            continue;
        }
        if let Some(chunk) = event.get("data").and_then(Value::as_str) {
            text.push_str(chunk);
        }
    }
    text
}

fn event_type(event: &Value) -> Option<&str> {
    event.get("type").and_then(Value::as_str)
}

fn serialized_size(value: &Value) -> u64 {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len() as u64)
        .unwrap_or(0)
}
