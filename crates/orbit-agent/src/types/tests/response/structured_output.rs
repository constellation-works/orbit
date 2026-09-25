#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;

use super::super::super::response::AgentResponseStatus;
use super::super::super::response::envelope::*;
use super::super::super::response::wrapper::provider_invocation_diagnostic;

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

/// Captured from Claude Code 2.1.220 invoked exactly the way the transport
/// now invokes it. Two details matter and both are load-bearing:
///
/// - the validated envelope appears in `structured_output` as an object
///   *and* in `result` as a JSON-encoded string, so a parser that only
///   knows `result` works by luck rather than by contract;
/// - `stop_reason` is `tool_use`, i.e. a tool-using run that would
///   previously have ended in prose was constrained into the envelope.
///   That is the ORB-10734 failure shape, prevented.
fn claude_json_schema_wrapper() -> String {
    serde_json::json!({
        "is_error": false,
        "duration_api_ms": 96110,
        "num_turns": 20,
        "stop_reason": "tool_use",
        "session_id": "44a7dbc8-333e-4852-aaf5-b61d8f4db174",
        "total_cost_usd": 0.24556440000000002,
        "usage": {
            "input_tokens": 154,
            "cache_creation_input_tokens": 19922,
            "cache_read_input_tokens": 714235,
            "output_tokens": 3372,
            "service_tier": "standard"
        },
        "modelUsage": {
            "claude-haiku-4-5-20251001": {
                "costUSD": 0.24556440000000002,
                "canonicalModel": "claude-haiku-4-5"
            }
        },
        "terminal_reason": "completed",
        "subtype": "success",
        "api_error_status": null,
        "result": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"summary\":\"done\"},\"error\":null}",
        "structured_output": {
            "schemaVersion": 1,
            "status": "success",
            "result": {"summary": "done"},
            "error": null
        }
    })
    .to_string()
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

/// Fail-closed: a `failed` envelope without a real error object is a
/// protocol violation, whether `error` is missing, JSON null, or the
/// string `"null"`. The [ORB-10770] coerce must not turn these into
/// success or synthesize a checkpointable failed envelope.
#[test]
fn failed_status_with_absent_error_tokens_is_a_protocol_violation() {
    let mut envelopes = vec![serde_json::json!({
        "schemaVersion": 1,
        "status": "failed",
        "result": {}
    })];
    for error in [serde_json::Value::Null, serde_json::json!("null")] {
        envelopes.push(serde_json::json!({
            "schemaVersion": 1,
            "status": "failed",
            "result": {},
            "error": error
        }));
    }

    for envelope in envelopes {
        let stdout = envelope.to_string();
        let error = parse_and_validate_response(&exec(&stdout, "", Some(1), false))
            .expect_err("failed without an error object is a protocol violation");
        assert!(
            error
                .to_string()
                .contains("failed status requires error object"),
            "envelope={envelope} error={error}"
        );
        assert!(
            synthesize_response(&exec(&stdout, "", Some(1), false)).is_none(),
            "must not synthesize a checkpointable failed envelope: {envelope}"
        );
    }
}

#[test]
fn claude_json_schema_wrapper_parses_and_keeps_its_trace_fields() {
    let stdout = claude_json_schema_wrapper();
    let (envelope, status, trace) = parse_and_validate_response(&exec(&stdout, "", Some(0), true))
        .expect("structured-output wrapper parses as an Orbit envelope");

    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.schema_version, 1);
    assert_eq!(envelope.result.expect("result")["summary"], "done");

    assert_eq!(trace.usage.input, 154);
    assert_eq!(trace.usage.output, 3372);
    assert_eq!(trace.usage.cache_read, 714_235);
    assert_eq!(trace.usage.cache_create, 19922);
    let cost = trace.provider_cost_usd.expect("provider cost");
    assert!((cost - 0.245_564_4).abs() < 1e-9, "{cost}");
    assert_eq!(
        trace.provider_model.as_deref(),
        Some("claude-haiku-4-5-20251001")
    );
    assert_eq!(trace.duration_ms, 96_110);
    // The provider's own `session_id` is preserved in the recorded stdout
    // blob; `InvocationTrace` has no field for it, and adding one would be
    // a new persisted artifact rather than part of this fix.
    assert!(stdout.contains("44a7dbc8-333e-4852-aaf5-b61d8f4db174"));
}

/// `structured_output` is the authoritative field, not a convenience copy.
/// Here `result` carries prose instead of the envelope — the shape the old
/// key-probe order could not survive.
#[test]
fn structured_output_outranks_the_result_string() {
    let stdout = serde_json::json!({
        "is_error": false,
        "subtype": "success",
        "terminal_reason": "completed",
        "result": "Here is a summary of what I did, in prose.",
        "structured_output": {
            "schemaVersion": 1,
            "status": "success",
            "result": {"authoritative": true},
            "error": null
        }
    })
    .to_string();

    let (envelope, status, _) = parse_and_validate_response(&exec(&stdout, "", Some(0), true))
        .expect("structured_output is read ahead of result");
    assert_eq!(status, AgentResponseStatus::Success);
    assert_eq!(envelope.result.expect("result")["authoritative"], true);
    assert!(response_envelope_protocol_check(&stdout).is_ok());
}

/// Captured `error_max_turns` ending: exit 0, no envelope anywhere, and
/// both `result` and `structured_output` null. Structured output removes
/// the *prose* cause of an exit-0 ending without an envelope, not the
/// category — so this still has to fail, but with a usable reason.
fn max_turns_wrapper() -> String {
    serde_json::json!({
        "is_error": true,
        "subtype": "error_max_turns",
        "terminal_reason": "max_turns",
        "num_turns": 200,
        "total_cost_usd": 4.17,
        "result": Option::<String>::None,
        "structured_output": Option::<String>::None
    })
    .to_string()
}

#[test]
fn a_turn_limit_ending_synthesizes_a_failed_envelope_naming_the_cause() {
    let stdout = max_turns_wrapper();
    let (envelope, status, trace) = parse_and_validate_response(&exec(&stdout, "", Some(0), true))
        .expect("an abnormal exit-0 ending synthesizes a failed envelope");

    assert_eq!(status, AgentResponseStatus::Failed);
    assert_eq!(envelope.status, "failed");
    let error = envelope.error.expect("synthesized error");
    assert_eq!(error.code, "AGENT_TERMINAL_ENDING");
    assert!(
        error.message.contains("error_max_turns"),
        "{}",
        error.message
    );
    assert!(error.message.contains("max_turns"), "{}", error.message);
    assert!(error.message.contains("is_error=true"), "{}", error.message);
    // Cost of the burnt run is still recovered for the scoreboard.
    assert_eq!(trace.provider_cost_usd, Some(4.17));
}

/// The completion guard's decision is unchanged; only its message improves.
#[test]
fn the_completion_guard_still_fails_and_now_names_the_terminal_reason() {
    let stdout = max_turns_wrapper();
    let error = response_envelope_protocol_check(&stdout)
        .expect_err("a run with no envelope must still fail the completion guard");
    let message = error.to_string();

    assert!(
        message.contains("does not contain an Orbit response envelope"),
        "the ORB-10449 invariant must stay recognizable: {message}"
    );
    assert!(message.contains("error_max_turns"), "{message}");
    assert!(message.contains("max_turns"), "{message}");
}

/// The ORB-10734 shape itself: an ordinary completion that simply answered
/// in prose. The wrapper reports nothing abnormal, so no cause is invented
/// and the generic message stands.
#[test]
fn an_ordinary_prose_ending_keeps_the_generic_message_and_synthesizes_nothing() {
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "terminal_reason": "completed",
        "stop_reason": "end_turn",
        "result": "QA sweep complete. Summary: no defects found."
    })
    .to_string();

    let error = response_envelope_protocol_check(&stdout).expect_err("no envelope");
    assert!(
        error
            .to_string()
            .ends_with("stdout does not contain an Orbit response envelope"),
        "a normal completion must not gain a fabricated cause: {error}"
    );
    assert!(
        synthesize_response(&exec(&stdout, "", Some(0), true)).is_none(),
        "a clean ending with no abnormal signal stays a hard parse failure"
    );
}

/// A CLI without the flag never starts work: commander rejects the unknown
/// option at argv parse. The point of failing here is that it costs
/// nothing, so the diagnostic has to be specific enough to act on.
#[test]
fn a_cli_lacking_the_flag_is_reported_as_a_capability_failure() {
    let diagnostic = provider_invocation_diagnostic("", "error: unknown option '--json-schema'\n")
        .expect("missing flag is diagnosable from stderr");

    assert!(
        diagnostic.contains("does not support --json-schema"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("no agent work ran"), "{diagnostic}");
    assert!(
        diagnostic.contains("rather than running unconstrained"),
        "{diagnostic}"
    );
}

/// A CLI that *has* the flag but whose API rejects the schema fails
/// mid-run instead, so the evidence is in the wrapper. Note `subtype`
/// still reads `"success"` here — keying on it would misclassify this as
/// a clean run.
#[test]
fn a_rejected_schema_is_diagnosed_from_the_wrapper_not_from_argv() {
    let stdout = serde_json::json!({
        "is_error": true,
        "subtype": "success",
        "structured_output": Option::<String>::None,
        "result": "API Error: 400 tools.0.custom.input_schema: input_schema does not support \
                   oneOf, allOf, or anyOf at the top level"
    })
    .to_string();

    assert!(
        provider_invocation_diagnostic("", "").is_none(),
        "nothing is wrong with the argv, so stderr cannot explain this"
    );
    let diagnostic =
        provider_invocation_diagnostic(&stdout, "").expect("wrapper explains the rejection");
    assert!(
        diagnostic.contains("rejected Orbit's response-envelope schema"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("input_schema"), "{diagnostic}");
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
