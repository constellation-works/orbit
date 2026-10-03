#![allow(missing_docs)]

use crate::providers::normalize_cli_stdout;

use serde_json::json;

const ORBIT_SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

/// The literal example envelope every Orbit prompt embeds in its response
/// contract. An OpenCode agent that reads its own task record or echoes the
/// prompt through a shell tool replays this inside a `tool_use` frame, so a
/// normalizer that read the whole stream could mistake Orbit's own instructions
/// for the agent's completion evidence. [ORB-11295]
const PROMPT_ECHO_ENVELOPE: &str =
    r#"{"schemaVersion":1,"status":"success|failed|timeout","result":{},"error":null}"#;

const SESSION_ID: &str = "ses_7f3a";

fn event(kind: &str, data: serde_json::Value) -> serde_json::Value {
    let mut event = json!({
        "type": kind,
        "timestamp": 1_762_000_000_000i64,
        "sessionID": SESSION_ID,
    });
    let object = event.as_object_mut().expect("event object");
    for (key, value) in data.as_object().expect("data object") {
        object.insert(key.clone(), value.clone());
    }
    event
}

fn text_event(text: &str) -> serde_json::Value {
    event(
        "text",
        json!({"part": {
            "id": "prt_text",
            "type": "text",
            "sessionID": SESSION_ID,
            "text": text,
            "time": {"start": 1_762_000_000_000i64, "end": 1_762_000_000_500i64},
        }}),
    )
}

/// A full OpenCode `--format json` stream for one successful turn, in the
/// documented event order.
fn opencode_stream(final_text: &str) -> String {
    [
        event(
            "step_start",
            json!({"part": {"id": "prt_step", "type": "step-start", "sessionID": SESSION_ID}}),
        ),
        event(
            "reasoning",
            json!({"part": {
                "id": "prt_reason",
                "type": "reasoning",
                "sessionID": SESSION_ID,
                // A draft envelope written while thinking aloud is not the
                // answer and must not be readable as one.
                "text": PROMPT_ECHO_ENVELOPE,
                "time": {"start": 1i64, "end": 2i64},
            }}),
        ),
        event(
            "tool_use",
            json!({"part": {
                "id": "prt_tool",
                "type": "tool",
                "tool": "bash",
                "sessionID": SESSION_ID,
                "state": {
                    "status": "completed",
                    "input": {"command": "orbit task show ORB-1"},
                    "output": PROMPT_ECHO_ENVELOPE,
                },
            }}),
        ),
        text_event(final_text),
        event(
            "step_finish",
            json!({"part": {
                "id": "prt_stepend",
                "type": "step-finish",
                "sessionID": SESSION_ID,
                "tokens": {"input": 120, "output": 40},
            }}),
        ),
    ]
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn tool_and_reasoning_frames_are_never_read_as_completion_evidence() {
    // Same stream with the assistant's answer removed: the run produced tool
    // traffic and thinking that both echo Orbit's own prompt, but no answer.
    let stdout = opencode_stream(ORBIT_SUCCESS)
        .lines()
        .filter(|line| !line.contains(r#""type":"text""#))
        .collect::<Vec<_>>()
        .join("\n");

    let normalized = normalize_cli_stdout("opencode", stdout.as_bytes());
    assert!(normalized.is_empty());
    assert!(!String::from_utf8_lossy(&normalized).contains("schemaVersion"));
}

#[test]
fn a_terminal_error_frame_invalidates_prior_answer_text() {
    // OpenCode also exits 1 here, and the v2 runner fails closed on a non-zero
    // exit, but normalization must not depend on the caller checking status.
    let stdout = format!(
        "{}\n{}",
        text_event(ORBIT_SUCCESS),
        event(
            "error",
            json!({"error": {
                "name": "ProviderAuthError",
                "data": {"message": "no credentials for provider anthropic"},
            }}),
        ),
    );

    assert!(
        normalize_cli_stdout("opencode", stdout.as_bytes()).is_empty(),
        "a terminal error frame must carry no completion evidence",
    );
}

#[test]
fn a_malformed_text_frame_refuses_the_whole_stream() {
    for malformed in [
        // `part.text` absent.
        event(
            "text",
            json!({"part": {"id": "p", "type": "text", "sessionID": SESSION_ID}}),
        ),
        // `part.text` is not a string.
        event(
            "text",
            json!({"part": {"id": "p", "type": "text", "sessionID": SESSION_ID, "text": 42}}),
        ),
        // `part` is not a text part.
        event(
            "text",
            json!({"part": {"id": "p", "type": "reasoning", "sessionID": SESSION_ID, "text": "x"}}),
        ),
        // `part` absent entirely.
        event("text", json!({})),
    ] {
        let stdout = format!("{}\n{malformed}", text_event(ORBIT_SUCCESS));
        assert!(
            normalize_cli_stdout("opencode", stdout.as_bytes()).is_empty(),
            "malformed text frame must invalidate the stream: {malformed}",
        );
    }
}
