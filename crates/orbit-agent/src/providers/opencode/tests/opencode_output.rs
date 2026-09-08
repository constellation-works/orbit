#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;
use serde_json::json;

use crate::providers::normalize_cli_stdout;
use crate::types::{AgentResponseStatus, parse_and_validate_response};

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

fn exec_result(stdout: &str, stderr: &str, exit_code: i32) -> ExecutionResult {
    ExecutionResult {
        success: exit_code == 0,
        timed_out: false,
        stdout: String::from_utf8(normalize_cli_stdout("opencode", stdout.as_bytes()).into_owned())
            .expect("utf8 normalized stdout"),
        stderr: stderr.to_string(),
        exit_code: Some(exit_code),
        duration_ms: 1_200,
        output: None,
    }
}

#[test]
fn documented_stream_yields_the_assistant_answer_text() {
    let stdout = opencode_stream(ORBIT_SUCCESS);

    let (envelope, status, _) =
        parse_and_validate_response(&exec_result(&stdout, "", 0)).expect("parse success envelope");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.status, "success");
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
fn successive_answer_parts_concatenate_in_stream_order() {
    let stdout = format!(
        "{}\n{}",
        text_event(r#"{"schemaVersion":1,"status":"success","#),
        text_event(r#""result":{"edited":true},"error":null}"#),
    );

    let normalized = normalize_cli_stdout("opencode", stdout.as_bytes());
    assert_eq!(String::from_utf8_lossy(&normalized), ORBIT_SUCCESS);
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

#[test]
fn absent_or_unparseable_output_yields_no_completion_evidence() {
    for stdout in [
        String::new(),
        "not json".to_string(),
        "{\"type\":\"text\"".to_string(),
        // Only control-plane frames: no answer was ever produced.
        event("step_start", json!({"part": {"type": "step-start"}})).to_string(),
    ] {
        let normalized = normalize_cli_stdout("opencode", stdout.as_bytes());
        assert!(
            normalized.is_empty(),
            "stdout '{stdout}' must carry no completion evidence",
        );
        assert!(parse_and_validate_response(&exec_result(&stdout, "", 0)).is_err());
    }
}

#[test]
fn a_nonzero_exit_fails_even_when_the_stream_carried_an_answer() {
    let stdout = opencode_stream(ORBIT_SUCCESS);

    assert!(
        parse_and_validate_response(&exec_result(&stdout, "session error", 1)).is_err(),
        "exit status and envelope are independent evidence; both must be valid",
    );
}
