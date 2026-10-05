#![allow(missing_docs)]

//! Fixture shapes here are taken from a real `copilot --output-format json`
//! run (CLI 1.0.80, npm `@github/copilot`). The auth-failure fixture is a
//! verbatim capture; the success/cancellation shapes use the same frame
//! envelope and the event vocabulary the shipped bundle emits. [ORB-10946]

use crate::providers::normalize_cli_stdout;
use crate::types::parse_and_validate_response;
use orbit_types::tool::ExecutionResult;

fn normalized(stdout: &str) -> String {
    String::from_utf8(normalize_cli_stdout("copilot", stdout.as_bytes()).into_owned())
        .expect("utf8 normalized stdout")
}

fn exec_result(stdout: &str, stderr: &str, exit_code: i32) -> ExecutionResult {
    ExecutionResult {
        success: exit_code == 0,
        timed_out: false,
        stdout: normalized(stdout),
        stderr: stderr.to_string(),
        exit_code: Some(exit_code),
        duration_ms: 1_200,
        output: None,
    }
}

#[test]
fn prompt_echo_is_never_read_as_completion_evidence() {
    // The regression this normalization exists for. Orbit's own prompt embeds
    // the response contract, example envelope included, and Copilot echoes the
    // prompt back as a `user.message` frame. A run that produced no model
    // output must not be able to satisfy the completion contract with Orbit's
    // own instructions.
    let prompt_echo = serde_json::json!({
        "type": "user.message",
        "data": {"content": String::from_utf8(
            crate::providers::copilot::copilot_cli::CopilotCliTransport::new(None)
                .stdin(br#"{"schemaVersion":1,"input":{}}"#),
        ).expect("utf8 prompt")},
    });
    let stdout = format!("{prompt_echo}\n");

    let kept = normalized(&stdout);
    assert!(kept.is_empty(), "prompt echo must not survive: {kept}");

    let parsed = parse_and_validate_response(&exec_result(&stdout, "", 0));
    assert!(
        parsed.is_err(),
        "a prompt echo alone must not parse as a completed envelope"
    );
}
