#![allow(missing_docs)]

use crate::providers::normalize_cli_stdout;
use crate::types::{AgentResponseStatus, parse_and_validate_response};
use orbit_types::tool::ExecutionResult;

const ORBIT_SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

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
fn missing_terminal_evidence_never_normalizes_to_success() {
    for stdout in [
        serde_json::json!({"type":"result","subtype":"success","result":ORBIT_SUCCESS}).to_string(),
        serde_json::json!({"type":"assistant","result":ORBIT_SUCCESS}).to_string(),
        serde_json::json!({"type":"result","subtype":"success","is_error":false}).to_string(),
    ] {
        assert!(normalize_cli_stdout("cursor", stdout.as_bytes()).is_empty());
    }
}
