#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;

use super::super::antigravity_output::{
    antigravity_terminal_error, antigravity_terminal_error_diagnostic,
};
use crate::providers::normalize_cli_stdout;
use crate::types::{AgentResponseStatus, parse_and_validate_response};

const ORBIT_SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

fn exec_result(stdout: &str, stderr: &str, exit_code: i32) -> ExecutionResult {
    ExecutionResult {
        success: exit_code == 0,
        timed_out: false,
        stdout: String::from_utf8(
            normalize_cli_stdout("antigravity", stdout.as_bytes()).into_owned(),
        )
        .expect("utf8 normalized stdout"),
        stderr: stderr.to_string(),
        exit_code: Some(exit_code),
        duration_ms: 1_200,
        output: None,
    }
}

fn stream_success(response: &str) -> String {
    format!(
        "{}\n{}\n",
        r#"{"event":"init","conversation_id":"c1","init":{"cwd":"/tmp","tools":[],"permission_mode":"always-proceed"}}"#,
        serde_json::json!({
            "event": "result",
            "result": {
                "conversation_id": "c1",
                "status": "SUCCESS",
                "response": response,
                "duration_seconds": 1.2,
                "num_turns": 1,
                "usage": {
                    "input_tokens": 10415,
                    "output_tokens": 657,
                    "thinking_tokens": 616,
                    "cache_read_tokens": 8113,
                    "total_tokens": 11072
                }
            }
        })
    )
}

#[test]
fn stream_json_success_yields_the_embedded_orbit_envelope_and_usage() {
    let stdout = stream_success(ORBIT_SUCCESS);
    let (envelope, status, trace) =
        parse_and_validate_response(&exec_result(&stdout, "", 0)).expect("parse success envelope");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.status, "success");
    assert_eq!(trace.usage.input, 10415);
    assert_eq!(trace.usage.output, 657 + 616);
    assert_eq!(trace.usage.cache_read, 8113);
}

#[test]
fn json_output_success_is_also_accepted() {
    let stdout = serde_json::json!({
        "conversation_id": "c1",
        "status": "SUCCESS",
        "response": ORBIT_SUCCESS,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 4,
            "thinking_tokens": 0,
            "cache_read_tokens": 0,
            "total_tokens": 14
        }
    })
    .to_string();

    let (envelope, status, _) =
        parse_and_validate_response(&exec_result(&stdout, "", 0)).expect("parse json envelope");
    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.status, "success");
}

#[test]
fn error_terminal_status_never_normalizes_to_success() {
    let stdout = serde_json::json!({
        "event": "result",
        "result": {
            "status": "ERROR",
            "response": ORBIT_SUCCESS,
            "error": "authentication required"
        }
    })
    .to_string();
    let normalized = normalize_cli_stdout("antigravity", stdout.as_bytes());
    assert!(normalized.is_empty());
}

#[test]
fn malformed_or_missing_result_yields_no_completion_evidence() {
    for stdout in [
        "not json",
        r#"{"event":"init","init":{}}"#,
        r#"{"event":"result","result":{"status":"RUNNING","response":""}}"#,
        "",
    ] {
        assert!(normalize_cli_stdout("antigravity", stdout.as_bytes()).is_empty());
    }
}

#[test]
fn timeout_terminal_error_is_bounded_and_omits_response_transcript() {
    let response = format!(
        r#"{{"schemaVersion":1,"status":"success","result":{{"prompt":"do not leak"}},"error":null}} credential {secret}"#,
        secret = "agy-tenant-42-authorization-bearer-zzz"
    );
    let stdout = serde_json::json!({
        "event": "result",
        "result": {
            "status": "ERROR",
            "response": response,
            "error": "timeout waiting for response"
        }
    })
    .to_string();

    let error = antigravity_terminal_error(stdout.as_bytes()).expect("terminal error");
    assert_eq!(error, "timeout waiting for response");
    assert!(!error.contains("do not leak"));
    assert!(!error.contains("agy-tenant-42"));

    let diagnostic = antigravity_terminal_error_diagnostic("antigravity", stdout.as_bytes())
        .expect("diagnostic");
    assert!(diagnostic.contains("timeout waiting for response"));
    assert!(!diagnostic.contains(&response));
    assert!(antigravity_terminal_error_diagnostic("claude", stdout.as_bytes()).is_none());
}

#[test]
fn success_terminal_does_not_yield_an_error_diagnostic() {
    let stdout = stream_success(ORBIT_SUCCESS);
    assert!(antigravity_terminal_error(stdout.as_bytes()).is_none());
}

#[test]
fn long_terminal_error_is_truncated_without_copying_response() {
    let error = "x".repeat(500);
    let stdout = serde_json::json!({
        "event": "result",
        "result": {
            "status": "ERROR",
            "response": ORBIT_SUCCESS,
            "error": error
        }
    })
    .to_string();
    let extracted = antigravity_terminal_error(stdout.as_bytes()).expect("bounded error");
    assert_eq!(extracted.chars().count(), 401);
    assert!(extracted.ends_with('…'));
    assert!(!extracted.contains("edited"));
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
