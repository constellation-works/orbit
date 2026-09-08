#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;
use serde_json::json;

use crate::providers::normalize_cli_stdout;
use crate::types::{AgentResponseStatus, parse_and_validate_response};

const ORBIT_SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

/// The literal example envelope every Orbit prompt embeds in its response
/// contract. Pi replays the user turn inside `agent_end`, so a normalizer that
/// read the whole stream could mistake Orbit's own instructions for the
/// agent's completion evidence.
const PROMPT_ECHO_ENVELOPE: &str =
    r#"{"schemaVersion":1,"status":"success|failed|timeout","result":{},"error":null}"#;

fn text_block(text: &str) -> serde_json::Value {
    json!({"type": "text", "text": text})
}

fn assistant_message(content: serde_json::Value, stop_reason: &str) -> serde_json::Value {
    json!({
        "role": "assistant",
        "content": content,
        "api": "anthropic-messages",
        "provider": "anthropic",
        "model": "claude-sonnet-4-5",
        "usage": {"input": 12, "output": 34, "cacheRead": 0, "cacheWrite": 0},
        "stopReason": stop_reason,
        "timestamp": 1_762_000_000_000i64,
    })
}

/// A full, documented Pi `--mode json` stream for one successful turn.
fn pi_stream(final_text: &str) -> String {
    [
        json!({"type":"session","version":3,"id":"9a1","timestamp":"2026-09-05T18:00:00Z","cwd":"/work"}),
        json!({"type":"agent_start"}),
        json!({"type":"turn_start"}),
        json!({"type":"message_start","message":assistant_message(json!([]), "pending")}),
        json!({
            "type":"message_update",
            "usage":{"input":12,"output":1,"cacheRead":0,"cacheWrite":0},
            "assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"…"},
        }),
        json!({"type":"message_end","message":assistant_message(json!([text_block(final_text)]), "stop")}),
        json!({"type":"turn_end","message":assistant_message(json!([text_block(final_text)]), "stop"),"toolResults":[]}),
        json!({
            "type":"agent_end",
            "messages":[
                {"role":"user","content":[text_block(PROMPT_ECHO_ENVELOPE)],"timestamp":1_762_000_000_000i64},
                assistant_message(json!([text_block(final_text)]), "stop"),
            ],
        }),
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
        stdout: String::from_utf8(normalize_cli_stdout("pi", stdout.as_bytes()).into_owned())
            .expect("utf8 normalized stdout"),
        stderr: stderr.to_string(),
        exit_code: Some(exit_code),
        duration_ms: 1_200,
        output: None,
    }
}

#[test]
fn documented_stream_yields_the_final_assistant_message() {
    let stdout = pi_stream(ORBIT_SUCCESS);

    let (envelope, status, _) =
        parse_and_validate_response(&exec_result(&stdout, "", 0)).expect("parse success envelope");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.status, "success");
}

#[test]
fn the_replayed_user_prompt_is_never_read_as_completion_evidence() {
    // Same stream, but the agent produced no `message_end` at all: it stopped
    // mid-turn. Only the `agent_end` history — carrying Orbit's own prompt —
    // remains, and it must not survive normalization.
    let stdout = pi_stream(ORBIT_SUCCESS)
        .lines()
        .filter(|line| !line.contains(r#""type":"message_end""#))
        .collect::<Vec<_>>()
        .join("\n");

    let normalized = normalize_cli_stdout("pi", stdout.as_bytes());
    assert!(normalized.is_empty());
    assert!(!String::from_utf8_lossy(&normalized).contains("schemaVersion"));
}

#[test]
fn the_last_completed_assistant_message_wins() {
    let stdout = format!(
        "{}\n{}",
        json!({"type":"message_end","message":assistant_message(json!([text_block("intermediate")]), "toolUse")}),
        json!({"type":"message_end","message":assistant_message(json!([text_block(ORBIT_SUCCESS)]), "stop")}),
    );

    let normalized = normalize_cli_stdout("pi", stdout.as_bytes());
    assert_eq!(String::from_utf8_lossy(&normalized), ORBIT_SUCCESS);
}

#[test]
fn thinking_and_tool_call_blocks_are_dropped() {
    let content = json!([
        {"type": "thinking", "thinking": PROMPT_ECHO_ENVELOPE},
        {"type": "toolCall", "id": "t1", "name": "bash", "arguments": {}},
        text_block(ORBIT_SUCCESS),
    ]);
    let stdout =
        json!({"type":"message_end","message":assistant_message(content, "stop")}).to_string();

    let normalized = normalize_cli_stdout("pi", stdout.as_bytes());
    assert_eq!(String::from_utf8_lossy(&normalized), ORBIT_SUCCESS);
}

#[test]
fn failed_stop_reasons_never_normalize_to_success() {
    for stop_reason in ["error", "aborted"] {
        let stdout = json!({
            "type": "message_end",
            "message": assistant_message(json!([text_block(ORBIT_SUCCESS)]), stop_reason),
        })
        .to_string();

        let normalized = normalize_cli_stdout("pi", stdout.as_bytes());
        assert!(
            normalized.is_empty(),
            "stopReason '{stop_reason}' must carry no completion evidence",
        );
    }
}

#[test]
fn a_later_failed_or_malformed_assistant_terminal_frame_invalidates_prior_text() {
    let successful = json!({
        "type":"message_end",
        "message":assistant_message(json!([text_block(ORBIT_SUCCESS)]), "stop"),
    });
    let failed = |stop_reason| {
        json!({
            "type":"message_end",
            "message":assistant_message(json!([text_block("later text")]), stop_reason),
        })
    };

    for terminal in [
        failed("error"),
        failed("aborted"),
        json!({
            "type":"message_end",
            "message":{"role":"assistant","content":[text_block("later text")]},
        }),
        json!({
            "type":"message_end",
            "message":assistant_message(json!([]), "stop"),
        }),
    ] {
        let stdout = format!("{successful}\n{terminal}");
        assert!(
            normalize_cli_stdout("pi", stdout.as_bytes()).is_empty(),
            "later assistant terminal outcome must invalidate prior evidence: {terminal}",
        );
    }
}

#[test]
fn later_non_assistant_and_control_frames_do_not_replace_an_assistant_outcome() {
    let stdout = format!(
        "{}\n{}\n{}\n{}",
        json!({"type":"message_end","message":assistant_message(json!([text_block(ORBIT_SUCCESS)]), "stop")}),
        json!({"type":"message_end","message":{"role":"user","content":[text_block("prompt echo")],"stopReason":"stop"}}),
        json!({"type":"message_end","message":{"role":"tool","content":[text_block("tool output")],"stopReason":"stop"}}),
        json!({"type":"agent_end","messages":[]}),
    );

    let normalized = normalize_cli_stdout("pi", stdout.as_bytes());
    assert_eq!(String::from_utf8_lossy(&normalized), ORBIT_SUCCESS);
}

#[test]
fn malformed_or_incomplete_frames_yield_no_completion_evidence() {
    for stdout in [
        String::new(),
        "not json".to_string(),
        "{\"type\":\"message_end\"".to_string(),
        // No `message`.
        json!({"type":"message_end"}).to_string(),
        // A user message must never be mistaken for the assistant's answer.
        json!({"type":"message_end","message":{"role":"user","content":[text_block(ORBIT_SUCCESS)],"stopReason":"stop"}}).to_string(),
        // No `stopReason` — the frame is not the authoritative final message.
        json!({"type":"message_end","message":{"role":"assistant","content":[text_block(ORBIT_SUCCESS)]}}).to_string(),
        // `content` is not an array.
        json!({"type":"message_end","message":{"role":"assistant","content":ORBIT_SUCCESS,"stopReason":"stop"}}).to_string(),
        // A `text` block whose `text` is the wrong type.
        json!({"type":"message_end","message":assistant_message(json!([{"type":"text","text":42}]), "stop")}).to_string(),
        // Terminal frame with no text content at all.
        json!({"type":"message_end","message":assistant_message(json!([]), "stop")}).to_string(),
    ] {
        assert!(
            normalize_cli_stdout("pi", stdout.as_bytes()).is_empty(),
            "invalid Pi output must yield no evidence: {stdout}",
        );
    }
}

#[test]
fn a_partial_final_line_does_not_discard_an_earlier_complete_frame() {
    // Output truncation cuts the stream mid-line. The complete `message_end`
    // that preceded it is still authoritative; the fragment is skipped.
    let stdout = format!(
        "{}\n{{\"type\":\"agent_end\",\"messa",
        json!({"type":"message_end","message":assistant_message(json!([text_block(ORBIT_SUCCESS)]), "stop")}),
    );

    let normalized = normalize_cli_stdout("pi", stdout.as_bytes());
    assert_eq!(String::from_utf8_lossy(&normalized), ORBIT_SUCCESS);
}

#[test]
fn nonzero_exit_with_a_success_frame_still_fails() {
    let execution = exec_result(
        &pi_stream(ORBIT_SUCCESS),
        "Error: model request failed\n",
        1,
    );

    if let Ok((_, status, _)) = parse_and_validate_response(&execution) {
        assert_ne!(status, AgentResponseStatus::Success);
    }
}

#[test]
fn other_providers_are_returned_borrowed_and_unchanged() {
    let stdout = br#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;

    for provider in ["claude", "codex", "gemini", "grok", "ollama"] {
        let out = normalize_cli_stdout(provider, stdout);
        assert!(matches!(out, std::borrow::Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), stdout);
    }
}
