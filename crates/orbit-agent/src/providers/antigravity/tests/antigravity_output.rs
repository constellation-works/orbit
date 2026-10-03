#![allow(missing_docs)]

use super::super::antigravity_output::{
    antigravity_terminal_error, antigravity_terminal_error_diagnostic,
};
use crate::providers::normalize_cli_stdout;

const ORBIT_SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

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
