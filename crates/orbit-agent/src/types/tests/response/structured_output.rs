#![allow(missing_docs)]

use super::super::super::response::AgentResponseStatus;
use super::super::super::response::envelope::*;

use orbit_types::tool::ExecutionResult;

fn exec(stdout: &str, stderr: &str, exit_code: Option<i32>, success: bool) -> ExecutionResult {
    ExecutionResult {
        success,
        timed_out: false,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        exit_code,
        duration_ms: 96_110,
        output: None,
    }
}

/// The safety invariant, stated as a test: the wrapper fields this task
/// introduced are diagnostic only. No combination of them — and no exit
/// code — may manufacture a success.
#[test]
fn no_wrapper_signal_combination_can_synthesize_a_success() {
    for is_error in [true, false] {
        for subtype in ["success", "error_max_turns", "error_during_execution"] {
            for terminal_reason in ["completed", "max_turns", "cancelled"] {
                let stdout = serde_json::json!({
                    "is_error": is_error,
                    "subtype": subtype,
                    "terminal_reason": terminal_reason,
                    "result": "durable work was persisted and the task looks done"
                })
                .to_string();

                for exit_code in [Some(0), Some(1)] {
                    let synthesized =
                        synthesize_response(&exec(&stdout, "", exit_code, exit_code == Some(0)));
                    if let Some((envelope, status, _)) = synthesized {
                        assert_eq!(
                            status,
                            AgentResponseStatus::Failed,
                            "is_error={is_error} subtype={subtype} \
                             terminal_reason={terminal_reason} exit={exit_code:?}"
                        );
                        assert_eq!(envelope.status, "failed");
                    }
                    // The completion guard never passes without an
                    // envelope, whatever the wrapper claims.
                    assert!(response_envelope_protocol_check(&stdout).is_err());
                }
            }
        }
    }
}

/// Live `jrun-20260813-0451-3` shape: Claude's constrained decoder emitted
/// the JSON *string* `"null"` for `error` in both `structured_output` and
/// the embedded `result` string. Wrapper signals are a normal completion
/// (`is_error=false`, `subtype=success`, `terminal_reason=completed`,
/// `stop_reason=tool_use`). On agent-main this missed the envelope; after
/// [ORB-10770] it must parse as success and keep the inner `result`.
fn claude_json_schema_wrapper_error_string_null() -> String {
    let inner = serde_json::json!({
        "schemaVersion": 1,
        "status": "success",
        "result": {
            "task_id": "ORB-10761",
            "summary": "narrowed the checkoutless-client guard"
        },
        "error": "null"
    });
    serde_json::json!({
        "is_error": false,
        "stop_reason": "tool_use",
        "terminal_reason": "completed",
        "subtype": "success",
        "result": inner.to_string(),
        "structured_output": {
            "schemaVersion": 1,
            "status": "success",
            "result": {
                "task_id": "ORB-10761",
                "summary": "narrowed the checkoutless-client guard"
            },
            "error": "null"
        }
    })
    .to_string()
}

#[test]
fn claude_json_schema_wrapper_with_error_string_null_parses_as_success() {
    let stdout = claude_json_schema_wrapper_error_string_null();
    let (envelope, status, _) = parse_and_validate_response(&exec(&stdout, "", Some(0), true))
        .expect("live jrun-20260813-0451-3 wrapper with error string \"null\" is an envelope");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.status, "success");
    assert!(envelope.error.is_none(), "string \"null\" is absent error");
    let result = envelope.result.expect("inner result object is kept");
    assert_eq!(result["task_id"], "ORB-10761");
    assert_eq!(result["summary"], "narrowed the checkoutless-client guard");
    response_envelope_protocol_check(&stdout).expect("string-null error still satisfies the frame");
}
