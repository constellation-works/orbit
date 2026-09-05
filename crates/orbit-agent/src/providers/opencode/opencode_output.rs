//! Reducing OpenCode's `--format json` NDJSON event stream to the bytes
//! Orbit's response/envelope contract is allowed to read. [ORB-11295]
//!
//! With `--format json`, OpenCode writes one JSON object per line, each shaped
//! `{"type":…,"timestamp":…,"sessionID":…,…}`:
//!
//! ```text
//! {"type":"step_start","timestamp":1,"sessionID":"ses_1","part":{…}}
//! {"type":"tool_use","timestamp":2,"sessionID":"ses_1","part":{…}}
//! {"type":"text","timestamp":3,"sessionID":"ses_1","part":{"type":"text","text":"…"}}
//! {"type":"error","timestamp":4,"sessionID":"ses_1","error":{…}}
//! ```
//!
//! Orbit's envelope finder searches a provider's stdout in reverse for a
//! recognizable envelope, descending through wrapper objects and JSON-encoded
//! strings. Handing it the raw stream is unsafe here for two reasons.
//!
//! First, `tool_use` frames carry the full tool call and result. An Orbit run
//! whose agent reads its own task record, or echoes the prompt into a shell
//! command, replays Orbit's own execution envelope — including the literal
//! example envelope in the response contract — back inside a tool payload. That
//! is not model-authored completion evidence and must not be readable as one.
//!
//! Second, `reasoning` frames are the model thinking aloud. A draft envelope
//! written while reasoning is not the answer, and OpenCode emits reasoning and
//! answer text as separate part types precisely so they stay distinguishable.
//!
//! So this module keeps only `text` frames — the assistant's answer parts,
//! which OpenCode emits only once a part has completed (`part.time.end` is
//! set) — and concatenates them in stream order. Everything else is dropped:
//! the session control plane, tool traffic, reasoning, and step boundaries.
//!
//! A terminal `error` frame invalidates the whole turn. OpenCode also sets exit
//! code 1 in that case, and the v2 runner fails closed on a non-zero exit, but
//! normalization must not depend on the caller checking status: an `error`
//! frame anywhere in the stream clears the accumulated text so a partially
//! written envelope from before the failure cannot be projected as success.
//!
//! When nothing survives, the result is empty, and empty stdout carries no
//! envelope — which is exactly how a missing completion is meant to be
//! reported.

use serde_json::Value;

/// Return the model-authored answer text from an OpenCode `--format json`
/// stream.
///
/// Any malformed, failed, or absent shape normalizes to empty bytes and
/// therefore cannot satisfy Orbit's completion contract.
pub(crate) fn normalize_opencode_stdout(stdout: &[u8]) -> Vec<u8> {
    let stream = String::from_utf8_lossy(stdout);
    let mut answer = String::new();
    for line in stream.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // A line that is not JSON is not an OpenCode event. `--print-logs`
        // sends logs to stderr and never interleaves here, so the tolerant
        // choice — skip it — keeps a stray banner from discarding a valid
        // answer without ever letting non-event text reach the envelope
        // reader.
        let Ok(event) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("error") => return Vec::new(),
            Some("text") => match text_part_content(event.get("part")) {
                // A `text` event whose `part.text` is absent or not a string is
                // a malformed frame, not an empty answer. Refuse the whole
                // stream rather than returning the frames that did parse.
                None => return Vec::new(),
                Some(text) => answer.push_str(text),
            },
            _ => {}
        }
    }
    answer.into_bytes()
}

/// Extract the answer text carried by one `text` event's `part`.
///
/// `None` means the frame is malformed and the stream cannot be trusted.
fn text_part_content(part: Option<&Value>) -> Option<&str> {
    let part = part?.as_object()?;
    if part.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    part.get("text").and_then(Value::as_str)
}
