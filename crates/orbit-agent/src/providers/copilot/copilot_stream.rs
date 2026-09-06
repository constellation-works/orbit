//! Normalizing and projecting the Copilot CLI's JSONL agent-event stream.
//! [ORB-10946] [ORB-11348]
//!
//! `copilot --output-format json` documents its stdout as "JSONL, one JSON
//! object per line". Every line is an agent-event frame:
//!
//! ```text
//! {"type":"session.warning","data":{…},"ephemeral":true,"id":"…","timestamp":"…"}
//! {"type":"assistant.message","data":{"content":"…"},"id":"…","timestamp":"…"}
//! ```
//!
//! Orbit's envelope finder searches a provider's stdout in reverse for a
//! recognizable envelope, descending through wrapper objects and JSON-encoded
//! strings. Handed a raw Copilot stream, that search is unsafe in one specific
//! way: Copilot echoes the *prompt* back as a `user.message` event, and every
//! Orbit prompt embeds a literal example envelope in its response contract
//! (`{"schemaVersion":1,"status":"success|failed|timeout",…}`). A run that
//! produced no model output at all still carries that echo, so the finder
//! could read Orbit's own instructions back as if they were the agent's
//! completion evidence.
//!
//! So this module exposes two deliberately different views: normalization
//! retains model-authored frames for invocation telemetry and diagnostics,
//! while response projection retains only assistant message content. Both
//! drop Orbit's echoed prompt and session control plane. When no assistant
//! answer survives — including auth-failure and cancellation shapes — the
//! response projection is empty and carries no completion evidence.

/// Event-type prefixes whose frames are model output.
///
/// `assistant.` carries the agent's messages (`assistant.message`), reasoning,
/// and per-turn token accounting (`assistant.usage`) that Orbit's invocation
/// trace reads. `error.` carries the provider's own failure frames
/// (`error.exception`), which must reach the diagnostic path rather than be
/// silently discarded.
///
/// This is a prefix allowlist rather than an exact-type list so a new
/// `assistant.*` subtype in a later CLI release keeps flowing instead of being
/// dropped by a stale table — while `user.*` and `session.*`, the two families
/// that can replay Orbit's own prompt, stay excluded by construction.
const MODEL_OUTPUT_EVENT_PREFIXES: &[&str] = &["assistant.", "error."];

/// Reduce a Copilot JSONL stream to its model-authored frames.
///
/// Returns the retained frames as JSONL. Lines that are not valid JSON, frames
/// with no `type` string, and frames outside [`MODEL_OUTPUT_EVENT_PREFIXES`]
/// are dropped: a malformed or truncated stream degrades toward less trace
/// data, never toward attributing Orbit's own input to the provider.
pub(crate) fn normalize_copilot_stdout(stdout: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(stdout);
    let mut out = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if !is_model_output_frame(&value) {
            continue;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    out.into_bytes()
}

/// Project a Copilot event stream onto completed assistant message content.
///
/// Reasoning, usage, errors, and tool request arguments remain available in
/// the normalized invocation trace, but only `assistant.message.data.content`
/// can supply Orbit response/status fields. [ORB-11348]
pub(crate) fn project_copilot_response(stdout: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(stdout);
    let mut terminal_answer = None;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) != Some("assistant.message") {
            continue;
        }

        // Tool-only messages are intermediate turns, not answers. Every
        // other assistant message is terminal answer evidence, including an
        // empty or malformed content field: a later invalid answer must not
        // let an earlier envelope remain authoritative.
        if has_tool_requests(&value) {
            continue;
        }
        let content = value
            .pointer("/data/content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        terminal_answer = Some(content.as_bytes().to_vec());
    }
    terminal_answer.unwrap_or_default()
}

fn has_tool_requests(value: &serde_json::Value) -> bool {
    value
        .pointer("/data/toolRequests")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|requests| !requests.is_empty())
}

fn is_model_output_frame(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|event_type| {
            MODEL_OUTPUT_EVENT_PREFIXES
                .iter()
                .any(|prefix| event_type.starts_with(prefix))
        })
}
