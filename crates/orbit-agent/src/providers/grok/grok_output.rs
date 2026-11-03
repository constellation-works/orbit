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
    match grok_streaming_events(stdout) {
        GrokStream::Recognized(events) => {
            return Some(grok_answer_text(&events).into_bytes());
        }
        // A recognized stream with a malformed line is untrusted: yield an
        // empty answer so tool/reasoning payloads in the raw capture cannot
        // be rediscovered as the completion envelope.
        GrokStream::MalformedRecognized(_) => return Some(Vec::new()),
        GrokStream::Unrecognized => {}
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
/// recognized stream, or a clean stream was cut off before `end`. A malformed
/// recognized stream reduces to an empty-text wrapper (plus any `end` spend
/// fields) so the raw capture is never rescanned for an envelope.
pub(crate) fn normalize_grok_stdout(stdout: &[u8]) -> Option<Vec<u8>> {
    let (events, malformed) = match grok_streaming_events(stdout) {
        GrokStream::Recognized(events) => (events, false),
        GrokStream::MalformedRecognized(events) => (events, true),
        GrokStream::Unrecognized => return None,
    };
    let end = events
        .iter()
        .rev()
        .find(|event| event_type(event) == Some("end"));
    if !malformed {
        let end = end?;
        let mut wrapper = Map::new();
        wrapper.insert("text".to_string(), Value::String(grok_answer_text(&events)));
        return finish_grok_wrapper(wrapper, end);
    }
    // Malformed recognized stream: never leave the raw capture for generic
    // scanning. Keep only the `end` event's spend fields, with no answer.
    let mut wrapper = Map::new();
    wrapper.insert("text".to_string(), Value::String(String::new()));
    if let Some(end) = end {
        return finish_grok_wrapper(wrapper, end);
    }
    serde_json::to_vec(&Value::Object(wrapper)).ok()
}

fn finish_grok_wrapper(mut wrapper: Map<String, Value>, end: &Value) -> Option<Vec<u8>> {
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
    let events = match grok_streaming_events(stdout) {
        GrokStream::Recognized(events) | GrokStream::MalformedRecognized(events) => events,
        GrokStream::Unrecognized => return None,
    };
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

enum GrokStream {
    /// More than one JSON object, at least one `end` or named `tool_call`,
    /// and every non-empty line a JSON object.
    Recognized(Vec<Value>),
    /// At least one Grok event type was seen, but a non-empty line was not
    /// a JSON object. The parsed events are kept for telemetry only.
    MalformedRecognized(Vec<Value>),
    Unrecognized,
}

const GROK_EVENT_TYPES: [&str; 6] = [
    "text",
    "thought",
    "tool_call",
    "tool_call_update",
    "usage",
    "end",
];

fn grok_streaming_events(stdout: &[u8]) -> GrokStream {
    // A complete JSON document belongs to the wrapper/direct-envelope path.
    // Its nested objects can occupy event-shaped lines without being JSONL.
    if serde_json::from_slice::<Value>(stdout).is_ok() {
        return GrokStream::Unrecognized;
    }

    let mut events = Vec::new();
    let mut recognized = false;
    let mut saw_grok_type = false;
    let mut malformed = false;
    for line in stdout.split(|byte| *byte == b'\n') {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            malformed = true;
            continue;
        };
        if !value.is_object() {
            malformed = true;
            continue;
        }
        saw_grok_type |= matches!(event_type(&value), Some(t) if GROK_EVENT_TYPES.contains(&t));
        recognized |= match event_type(&value) {
            Some("end") => true,
            Some("tool_call") => value.get("toolName").and_then(Value::as_str).is_some(),
            _ => false,
        };
        events.push(value);
    }
    if malformed {
        if saw_grok_type {
            return GrokStream::MalformedRecognized(events);
        }
        return GrokStream::Unrecognized;
    }
    if events.len() > 1 && recognized {
        GrokStream::Recognized(events)
    } else {
        GrokStream::Unrecognized
    }
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
