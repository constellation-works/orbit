#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;

use crate::providers::project_cli_response;
use crate::types::{AgentResponseStatus, parse_and_validate_response};

const SUCCESS: &str =
    r#"{"schemaVersion":1,"status":"success","result":{"source":"final-text"},"error":null}"#;
const FAILURE: &str = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"final_failure","message":"the final answer failed","details":null}}"#;
const TIMEOUT: &str = r#"{"schemaVersion":1,"status":"timeout","result":{},"error":{"code":"final_timeout","message":"the final answer timed out","details":null}}"#;

fn projected(stdout: &str) -> String {
    String::from_utf8(project_cli_response("grok", stdout.as_bytes()).into_owned())
        .expect("utf8 projected response")
}

fn exec_result(stdout: String, exit_code: i32) -> ExecutionResult {
    ExecutionResult {
        success: exit_code == 0,
        timed_out: false,
        stdout,
        stderr: String::new(),
        exit_code: Some(exit_code),
        duration_ms: 1,
        output: None,
    }
}

#[test]
fn grok_final_text_is_the_only_completion_authority() {
    let stdout = serde_json::json!({
        "text": SUCCESS,
        "stopReason": "EndTurn",
        "thought": format!("ignored reasoning: {FAILURE}"),
        "toolCalls": [{"result": FAILURE}],
        "usage": {"metadata": SUCCESS},
        "sessionId": "session-fixture",
        "requestId": "request-fixture",
    })
    .to_string();

    let response = projected(&stdout);
    let (envelope, status, _) =
        parse_and_validate_response(&exec_result(response, 0)).expect("final text parses");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.result.expect("result")["source"], "final-text");
}

#[test]
fn grok_final_failure_overrides_success_shaped_metadata() {
    let stdout = serde_json::json!({
        "text": FAILURE,
        "stopReason": "EndTurn",
        "thought": SUCCESS,
        "usage": {"result": SUCCESS},
    })
    .to_string();

    let (envelope, status, _) = parse_and_validate_response(&exec_result(projected(&stdout), 1))
        .expect("final failure parses");

    assert_eq!(status, AgentResponseStatus::Failed);
    assert_eq!(
        envelope.error.expect("failure detail").code,
        "final_failure"
    );
}

#[test]
fn grok_final_timeout_overrides_success_shaped_metadata() {
    let stdout = serde_json::json!({
        "text": TIMEOUT,
        "stopReason": "EndTurn",
        "thought": SUCCESS,
        "usage": {"result": SUCCESS},
    })
    .to_string();
    let mut execution = exec_result(projected(&stdout), 1);
    execution.timed_out = true;

    let (envelope, status, _) =
        parse_and_validate_response(&execution).expect("final timeout parses");

    assert_eq!(status, AgentResponseStatus::Timeout);
    assert_eq!(envelope.status, "timeout");
}

#[test]
fn malformed_or_missing_grok_final_text_has_no_completion_evidence() {
    for wrapper in [
        serde_json::json!({
            "stopReason": "EndTurn",
            "thought": SUCCESS,
            "toolCalls": [{"result": SUCCESS}],
        }),
        serde_json::json!({
            "text": {"unexpected": SUCCESS},
            "stopReason": "EndTurn",
            "usage": {"result": SUCCESS},
        }),
    ] {
        assert!(projected(&wrapper.to_string()).is_empty());
    }
}

#[test]
fn direct_and_streaming_envelopes_keep_the_existing_parser_boundary() {
    for stdout in [SUCCESS.to_string(), format!("{SUCCESS}\n{SUCCESS}")] {
        assert_eq!(projected(&stdout), stdout);
    }
}

#[test]
fn other_provider_responses_are_unchanged() {
    for provider in ["claude", "gemini", "ollama"] {
        let projected = project_cli_response(provider, SUCCESS.as_bytes());

        assert!(matches!(projected, std::borrow::Cow::Borrowed(_)));
        assert_eq!(projected.as_ref(), SUCCESS.as_bytes());
    }
}
