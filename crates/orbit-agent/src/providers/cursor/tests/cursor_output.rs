#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;

use crate::providers::normalize_cli_stdout;
use crate::types::{AgentResponseStatus, parse_and_validate_response};

const ORBIT_SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

/// Captured from Cursor Agent CLI 2026.08.11-e8db854 before authentication on
/// a fresh host state. Like documented auth/API failures, the CLI exits 1,
/// writes the error to stderr, and emits no JSON success object on stdout.
const REAL_STATE_FAILURE_STDERR: &str =
    "Error: ENOENT: no such file or directory, mkdir '/home/example/.cursor/projects/tmp'\n";

fn cursor_result(result: serde_json::Value) -> String {
    serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "duration_ms": 1234,
        "duration_api_ms": 987,
        "result": result,
        "session_id": "2fa6a0fb-7cf5-4f7e-9d41-e91f6f6b3333",
        "request_id": "req-123",
    })
    .to_string()
}

fn exec_result(stdout: &str, stderr: &str, exit_code: i32) -> ExecutionResult {
    ExecutionResult {
        success: exit_code == 0,
        timed_out: false,
        stdout: String::from_utf8(normalize_cli_stdout("cursor", stdout.as_bytes()).into_owned())
            .expect("utf8 normalized stdout"),
        stderr: stderr.to_string(),
        exit_code: Some(exit_code),
        duration_ms: 1_200,
        output: None,
    }
}

#[test]
fn documented_success_wrapper_yields_the_embedded_orbit_envelope() {
    let stdout = cursor_result(serde_json::Value::String(ORBIT_SUCCESS.to_string()));

    let (envelope, status, _) =
        parse_and_validate_response(&exec_result(&stdout, "", 0)).expect("parse success envelope");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.status, "success");
}

#[test]
fn failure_wrapper_never_normalizes_to_success() {
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "error",
        "is_error": true,
        "result": ORBIT_SUCCESS,
    })
    .to_string();

    let normalized = normalize_cli_stdout("cursor", stdout.as_bytes());
    assert!(normalized.is_empty());
    if let Ok((_, status, _)) =
        parse_and_validate_response(&exec_result(&stdout, "provider failed", 1))
    {
        assert_ne!(status, AgentResponseStatus::Success);
    }
}

#[test]
fn captured_real_nonzero_failure_has_no_completion_evidence() {
    let execution = exec_result("", REAL_STATE_FAILURE_STDERR, 1);
    assert!(execution.stdout.is_empty());
    if let Ok((_, status, _)) = parse_and_validate_response(&execution) {
        assert_ne!(status, AgentResponseStatus::Success);
    }
}

#[test]
fn missing_terminal_evidence_never_normalizes_to_success() {
    for stdout in [
        serde_json::json!({"type":"result","subtype":"success","result":ORBIT_SUCCESS}).to_string(),
        serde_json::json!({"type":"assistant","result":ORBIT_SUCCESS}).to_string(),
        serde_json::json!({"type":"result","subtype":"success","is_error":false}).to_string(),
    ] {
        assert!(normalize_cli_stdout("cursor", stdout.as_bytes()).is_empty());
    }
}

#[test]
fn malformed_output_yields_no_completion_evidence() {
    for stdout in ["not json", "{\"type\":\"result\"", ""] {
        assert!(normalize_cli_stdout("cursor", stdout.as_bytes()).is_empty());
    }
}

#[test]
fn non_string_result_is_rejected() {
    let stdout = cursor_result(serde_json::json!({"schemaVersion":1,"status":"success"}));
    assert!(normalize_cli_stdout("cursor", stdout.as_bytes()).is_empty());
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
