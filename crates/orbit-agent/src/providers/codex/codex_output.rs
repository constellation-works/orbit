//! Projects Codex CLI JSONL onto completed assistant answer text.
//!
//! `codex exec --json` interleaves assistant messages with reasoning, command
//! executions, and usage events. Only completed `agent_message` item text is
//! an assistant answer; every other event remains raw invocation telemetry but
//! must not become Orbit response/status evidence. [ORB-11348]

/// Return answer-only bytes when `stdout` is a recognized Codex JSONL stream.
///
/// `None` means the capture is not Codex event JSONL and lets legacy direct
/// envelope output retain the provider-agnostic fallback behavior.
pub(crate) fn project_codex_response(stdout: &[u8]) -> Option<Vec<u8>> {
    let text = String::from_utf8_lossy(stdout);
    let mut saw_codex_event = false;
    let mut answer = String::new();

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(event_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !is_codex_event(event_type) {
            continue;
        }
        saw_codex_event = true;

        if event_type != "item.completed"
            || value
                .pointer("/item/type")
                .and_then(serde_json::Value::as_str)
                != Some("agent_message")
        {
            continue;
        }
        let Some(message) = value
            .pointer("/item/text")
            .and_then(serde_json::Value::as_str)
            .filter(|message| !message.is_empty())
        else {
            continue;
        };
        answer.push_str(message);
        answer.push('\n');
    }

    saw_codex_event.then(|| answer.into_bytes())
}

fn is_codex_event(event_type: &str) -> bool {
    event_type.starts_with("thread.")
        || event_type.starts_with("turn.")
        || event_type.starts_with("item.")
        || event_type == "error"
}
