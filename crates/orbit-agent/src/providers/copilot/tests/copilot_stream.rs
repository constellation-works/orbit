#![allow(missing_docs)]

//! Fixture shapes here are taken from a real `copilot --output-format json`
//! run (CLI 1.0.80, npm `@github/copilot`). The auth-failure fixture is a
//! verbatim capture; the success/cancellation shapes use the same frame
//! envelope and the event vocabulary the shipped bundle emits. [ORB-10946]

use orbit_types::tool::ExecutionResult;

use crate::providers::{normalize_cli_stdout, project_cli_response};
use crate::types::{AgentResponseStatus, parse_and_validate_response};

/// Verbatim stdout from a real Copilot 1.0.80 run whose token lacked Copilot
/// entitlement: session control-plane events only, the failure itself on
/// stderr, exit 1. No `assistant.message`, so no completion evidence.
const REAL_AUTH_FAILURE_STDOUT: &str = concat!(
    r#"{"type":"session.warning","data":{"message":"Third-party MCP servers are disabled by your organization's Copilot policy. Only built-in servers are available.","warningType":"policy"},"ephemeral":true,"id":"813be348","timestamp":"2026-08-22T02:34:53.964Z"}"#,
    "\n",
    r#"{"type":"session.mcp_servers_loaded","data":{"servers":[]},"ephemeral":true,"id":"262f9cf2","timestamp":"2026-08-22T02:34:53.971Z"}"#,
    "\n",
    r#"{"type":"session.mcp_server_status_changed","data":{"serverName":"github-mcp-server","status":"connected"},"ephemeral":true,"id":"2a233069","timestamp":"2026-08-22T02:34:54.961Z"}"#,
    "\n",
);

const REAL_AUTH_FAILURE_STDERR: &str = "Error: Authentication failed (Request ID: AE1C:B51BD)\n\nYour GitHub token may be invalid, expired, or lacking the required permissions.\n";

fn envelope_line(content: &str) -> String {
    let frame = serde_json::json!({
        "type": "assistant.message",
        "data": {"content": content},
        "id": "msg-1",
        "timestamp": "2026-08-22T02:34:59.000Z",
    });
    format!("{frame}\n")
}

fn normalized(stdout: &str) -> String {
    String::from_utf8(normalize_cli_stdout("copilot", stdout.as_bytes()).into_owned())
        .expect("utf8 normalized stdout")
}

fn projected(stdout: &str) -> String {
    String::from_utf8(project_cli_response("copilot", stdout.as_bytes()).into_owned())
        .expect("utf8 projected response")
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
fn success_stream_yields_the_embedded_orbit_envelope() {
    let stdout = format!(
        "{}{}{}",
        r#"{"type":"session.created","data":{"sessionId":"s1"},"ephemeral":true}"#.to_string()
            + "\n",
        envelope_line(
            r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#
        ),
        r#"{"type":"session.idle","data":{},"ephemeral":true}"#.to_string() + "\n",
    );

    let (envelope, status, _) =
        parse_and_validate_response(&exec_result(&stdout, "", 0)).expect("parse success envelope");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.status, "success");
}

#[test]
fn assistant_usage_survives_normalization_for_the_invocation_trace() {
    // Token accounting rides on `assistant.usage`. Normalization must not cost
    // Orbit its telemetry, which is why the filter is an `assistant.*` prefix
    // allowlist rather than "keep only the final message".
    let stdout = format!(
        "{}{}",
        r#"{"type":"assistant.usage","data":{"inputTokens":120,"outputTokens":34}}"#.to_string()
            + "\n",
        envelope_line(r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#),
    );

    let kept = normalized(&stdout);

    assert!(kept.contains("assistant.usage"));
    assert!(kept.contains("assistant.message"));
}

#[test]
fn reasoning_cannot_supply_the_assistant_answer() {
    let stdout = concat!(
        r#"{"type":"assistant.reasoning","data":{"content":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"claimed\":\"reasoning\"},\"error\":null}"},"id":"reason-1"}"#,
        "\n",
        r#"{"type":"assistant.usage","data":{"inputTokens":120,"outputTokens":34}}"#,
        "\n",
    );

    assert!(projected(stdout).is_empty());
    assert!(normalized(stdout).contains("assistant.usage"));
}

#[test]
fn empty_message_tool_arguments_cannot_supply_the_assistant_answer() {
    let stdout = concat!(
        r#"{"type":"assistant.message","data":{"content":"","toolRequests":[{"toolCallId":"call-1","name":"shell","arguments":{"command":"printf '%s' '{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"claimed\":\"tool-arguments\"},\"error\":null}'"}}]},"id":"msg-1"}"#,
        "\n",
        r#"{"type":"session.abort","data":{"reason":"cancelled"},"ephemeral":true}"#,
        "\n",
    );

    assert!(projected(stdout).is_empty());
    assert!(normalized(stdout).contains("toolRequests"));
}

#[test]
fn final_message_after_tool_traffic_is_the_only_projected_content() {
    let stdout = concat!(
        r#"{"type":"assistant.message","data":{"content":"","toolRequests":[{"toolCallId":"call-1","name":"shell","arguments":{"command":"echo {\"schemaVersion\":1,\"status\":\"failed\"}"}}]},"id":"msg-1"}"#,
        "\n",
        r#"{"type":"assistant.message","data":{"content":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"source\":\"assistant\"},\"error\":null}"},"id":"msg-2"}"#,
        "\n",
    );

    let answer = projected(stdout);
    assert!(answer.contains(r#""source":"assistant""#));
    assert!(!answer.contains("toolCallId"));
}

#[test]
fn terminal_assistant_message_replaces_earlier_commentary() {
    let envelope = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"fixture","message":"failed"}}"#;
    let stdout = format!(
        "{}{}",
        envelope_line("Commentary: I updated the files."),
        envelope_line(envelope),
    );

    assert_eq!(projected(&stdout), envelope);
}

#[test]
fn trailing_assistant_message_replaces_an_earlier_envelope() {
    let stdout = format!(
        "{}{}",
        envelope_line(r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#),
        envelope_line("Courtesy: the run is complete."),
    );

    assert_eq!(projected(&stdout), "Courtesy: the run is complete.");
}

#[test]
fn empty_or_missing_terminal_content_replaces_an_earlier_envelope() {
    let envelope = envelope_line(
        r#"{"schemaVersion":1,"status":"success","result":{"source":"earlier"},"error":null}"#,
    );

    for terminal_message in [
        r#"{"type":"assistant.message","data":{"content":""},"id":"msg-2"}"#,
        r#"{"type":"assistant.message","data":{},"id":"msg-3"}"#,
    ] {
        assert!(
            projected(&(envelope.clone() + terminal_message + "\n")).is_empty(),
            "terminal message must replace the earlier envelope: {terminal_message}",
        );
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

#[test]
fn real_auth_failure_capture_reports_no_completion_evidence() {
    assert!(normalized(REAL_AUTH_FAILURE_STDOUT).is_empty());

    let result = parse_and_validate_response(&exec_result(
        REAL_AUTH_FAILURE_STDOUT,
        REAL_AUTH_FAILURE_STDERR,
        1,
    ));

    if let Ok((_, status, _)) = result {
        assert_ne!(
            status,
            AgentResponseStatus::Success,
            "a failed launch must never normalize to success"
        );
    }
}

#[test]
fn cancellation_stream_does_not_normalize_to_success() {
    // A cancelled turn ends on the session control plane with no terminal
    // assistant message, so it carries no completion evidence either.
    let stdout = format!(
        "{}{}",
        r#"{"type":"assistant.turn_start","data":{},"id":"t1"}"#.to_string() + "\n",
        r#"{"type":"session.abort","data":{"reason":"cancelled"},"ephemeral":true}"#.to_string()
            + "\n",
    );

    let kept = normalized(&stdout);
    assert!(!kept.contains("session.abort"));

    let result = parse_and_validate_response(&exec_result(&stdout, "", 130));
    if let Ok((_, status, _)) = result {
        assert_ne!(status, AgentResponseStatus::Success);
    }
}

#[test]
fn malformed_lines_are_dropped_without_taking_the_envelope_with_them() {
    // A truncated capture or interleaved non-JSON chatter must not cost the
    // run its envelope, and must not itself become evidence.
    let stdout = format!(
        "{}{}{}",
        "not json at all\n",
        envelope_line(
            r#"{"schemaVersion":1,"status":"success","result":{"ok":true},"error":null}"#
        ),
        r#"{"type":"assistant.message","data":{"content":"tru"#.to_string() + "\n",
    );

    let (_, status, _) =
        parse_and_validate_response(&exec_result(&stdout, "", 0)).expect("parse envelope");

    assert_eq!(status, AgentResponseStatus::Success);
}

#[test]
fn wholly_malformed_stream_yields_nothing() {
    let stdout = "not json\n{ still not json\n";

    assert!(normalized(stdout).is_empty());
}

#[test]
fn other_providers_are_returned_borrowed_and_unchanged() {
    let stdout = br#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;

    for provider in ["claude", "codex", "gemini", "grok", "ollama"] {
        let out = normalize_cli_stdout(provider, stdout);
        assert!(
            matches!(out, std::borrow::Cow::Borrowed(_)),
            "{provider} must not be reshaped"
        );
        assert_eq!(out.as_ref(), stdout);
    }
}
