//! Reducing Pi's `--mode json` JSONL event stream to the bytes Orbit's
//! response/envelope contract is allowed to read. [ORB-11296]
//!
//! Pi documents its JSON mode as one JSON object per line, beginning with a
//! session header and followed by agent/turn/message/tool lifecycle events:
//!
//! ```text
//! {"type":"session","version":3,"id":"…","timestamp":"…","cwd":"/path"}
//! {"type":"agent_start"}
//! {"type":"message_end","message":{"role":"assistant","content":[…],"stopReason":"stop",…}}
//! {"type":"agent_end","messages":[…]}
//! ```
//!
//! Orbit's envelope finder searches a provider's stdout in reverse for a
//! recognizable envelope, descending through wrapper objects and JSON-encoded
//! strings. Handed a raw Pi stream, that search is unsafe in two specific ways.
//! First, Pi's `agent_end` frame carries `messages` — the *whole* conversation,
//! including the user turn, which is Orbit's own prompt with its literal
//! example envelope embedded in the response contract. Second, `message_start`
//! and `message_update` describe a message that is still being produced. Either
//! could be read as completion evidence for a run that produced none.
//!
//! So this module reduces the stream to the text of the single frame Pi
//! documents as authoritative — `message_end` — and emits only that. What
//! survives is what the model wrote; what is dropped is Orbit's own prompt
//! coming back, the session control plane, tool traffic, thinking blocks, and
//! the streaming deltas. When nothing survives, the result is empty, and an
//! empty stdout carries no envelope, which is exactly how a missing completion
//! is meant to be reported.

use serde_json::Value;

/// Stop reasons that mean the assistant turn did not produce a usable answer.
///
/// Pi's own text print mode treats exactly these two as failures: it writes
/// `errorMessage` to stderr and exits 1 instead of printing content. Orbit
/// mirrors that classification so a JSON-mode run cannot succeed where the
/// equivalent text-mode run would have failed.
const FAILED_STOP_REASONS: &[&str] = &["error", "aborted"];

/// Return the model-authored response text from the last authoritative
/// assistant message Pi emitted.
///
/// Any malformed, partial, failed, or absent shape normalizes to empty bytes
/// and therefore cannot satisfy Orbit's completion contract.
pub(crate) fn normalize_pi_stdout(stdout: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(stdout);
    let mut latest = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("message_end") {
            continue;
        }
        if let Some(rendered) = assistant_text(value.get("message")) {
            latest = Some(rendered);
        }
    }
    latest.map_or_else(Vec::new, String::into_bytes)
}

/// Concatenate the `text` content blocks of one completed assistant message.
///
/// Returns `None` unless the frame is a genuine assistant message that stopped
/// cleanly. `thinking` and `toolCall` blocks are deliberately skipped: they are
/// not the model's answer, and reasoning text in particular may quote the
/// prompt's example envelope verbatim.
fn assistant_text(message: Option<&Value>) -> Option<String> {
    let message = message?.as_object()?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let stop_reason = message.get("stopReason").and_then(Value::as_str)?;
    if FAILED_STOP_REASONS.contains(&stop_reason) {
        return None;
    }
    let mut rendered = String::new();
    for block in message.get("content")?.as_array()? {
        if block.get("type").and_then(Value::as_str) != Some("text") {
            continue;
        }
        let Some(text) = block.get("text").and_then(Value::as_str) else {
            // A `text` block whose `text` is absent or not a string is a
            // malformed frame, not an empty answer. Refuse the whole message
            // rather than silently returning the blocks that did parse.
            return None;
        };
        rendered.push_str(text);
    }
    (!rendered.is_empty()).then_some(rendered)
}
