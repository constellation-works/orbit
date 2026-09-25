#![allow(missing_docs)]

use orbit_types::tool::ExecutionResult;

use super::super::super::response::AgentResponseStatus;
use super::super::super::response::envelope::*;

fn exec(stdout: &str, stderr: &str, exit_code: Option<i32>, success: bool) -> ExecutionResult {
    ExecutionResult {
        success,
        timed_out: false,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        exit_code,
        duration_ms: 1234,
        output: None,
    }
}

#[test]
fn stderr_timeout_phrase_without_supervisor_verdict_is_invocation_failure() {
    let exec = exec("", "tool: process timed out", Some(1), false);

    let (envelope, status, _) = synthesize_response(&exec).expect("failure is synthesized");

    assert_eq!(status, AgentResponseStatus::Failed);
    assert_eq!(
        envelope.error.expect("failure error").code,
        "AGENT_INVOCATION_FAILED"
    );
}

#[test]
fn synthesize_trace_preserves_usage_from_provider_json_without_envelope() {
    // Provider-shaped JSON with usage but no Orbit envelope; agent failed
    // (non-zero exit) so the synthesize fallback runs. Token totals from
    // the outer JSON must survive instead of being zeroed.
    let stdout = r#"{"type":"result","usage":{"input_tokens":42,"output_tokens":7}}"#;
    let result = parse_and_validate_response(&exec(stdout, "", Some(1), false));
    // No envelope means the synthesize fallback only succeeds if stdout is
    // empty; with content but exit!=0, parse_and_validate returns Err. We
    // exercise synthesize_trace directly to verify the trace contents the
    // synthesize path WOULD return.
    assert!(result.is_err(), "expected envelope parse to fail");

    let trace = synthesize_trace(&exec(stdout, "", Some(1), false));
    assert_eq!(trace.usage.input, 42);
    assert_eq!(trace.usage.output, 7);
    assert_eq!(trace.duration_ms, 1234);
}

#[test]
fn synthesize_trace_preserves_claude_outer_usage_when_envelope_invalid() {
    // Mimics `claude -p --output-format json` output: outer `usage` plus
    // a `result` string that does NOT contain a valid Orbit envelope (e.g.
    // claude failed mid-flight and emitted free text). Outer usage must
    // still be captured.
    let stdout = r#"{"type":"result","subtype":"success","result":"plain text reply, not an envelope","usage":{"input_tokens":1000,"output_tokens":250,"cache_read_input_tokens":500,"cache_creation_input_tokens":100}}"#;
    let trace = synthesize_trace(&exec(stdout, "", Some(0), true));
    assert_eq!(trace.usage.input, 1000);
    assert_eq!(trace.usage.output, 250);
    assert_eq!(trace.usage.cache_read, 500);
    assert_eq!(trace.usage.cache_create, 100);
}

#[test]
fn claude_cli_selects_the_highest_cost_reported_model_and_cost() {
    // Captured from Claude Code 2.1.220 with `--model fable`: the CLI
    // reports both a small internal Haiku invocation and the requested
    // model. Per-model cost is the only provider-owned discriminator.
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "result": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}",
        "total_cost_usd": 0.286169,
        "modelUsage": {
            "claude-haiku-4-5-20251001": {
                "costUSD": 0.000598,
                "canonicalModel": "claude-haiku-4-5"
            },
            "claude-fable-5": {
                "costUSD": 0.285571,
                "canonicalModel": "claude-fable-5"
            }
        }
    })
    .to_string();

    let (_, _, trace) =
        parse_and_validate_response(&exec(&stdout, "", Some(0), true)).expect("Claude parses");
    assert_eq!(trace.provider_model.as_deref(), Some("claude-fable-5"));
    assert_eq!(trace.provider_cost_usd, Some(0.286169));
}

#[test]
fn claude_cli_leaves_ambiguous_equal_cost_models_unknown() {
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "result": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}",
        "modelUsage": {
            "claude-a": { "costUSD": 1.0 },
            "claude-b": { "costUSD": 1.0 }
        }
    })
    .to_string();

    let (_, _, trace) =
        parse_and_validate_response(&exec(&stdout, "", Some(0), true)).expect("Claude parses");
    assert_eq!(trace.provider_model, None);
}

#[test]
fn gemini_cli_reads_the_single_stats_model_key_without_a_cost() {
    // `stats.models` is the live Gemini CLI shape already used by the
    // token-ingest regression fixtures. Unlike Claude, it reports no
    // invocation-total USD cost.
    let stdout = serde_json::json!({
        "response": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}",
        "stats": {
            "models": {
                "gemini-3.1-pro": {
                    "tokens": {
                        "input": 40_919,
                        "output": 70,
                        "cached": 40_101,
                        "thoughts": 396,
                        "tool": 0,
                        "total": 41_385
                    }
                }
            }
        }
    })
    .to_string();

    let (_, _, trace) =
        parse_and_validate_response(&exec(&stdout, "", Some(0), true)).expect("Gemini parses");
    assert_eq!(trace.provider_model.as_deref(), Some("gemini-3.1-pro"));
    assert_eq!(trace.provider_cost_usd, None);
}

#[test]
fn gemini_cli_leaves_multiple_stats_models_unknown() {
    let stdout = serde_json::json!({
        "response": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}",
        "stats": {
            "models": {
                "gemini-3.1-pro": { "tokens": { "total": 10 } },
                "gemini-2.5-flash": { "tokens": { "total": 5 } }
            }
        }
    })
    .to_string();

    let (_, _, trace) =
        parse_and_validate_response(&exec(&stdout, "", Some(0), true)).expect("Gemini parses");
    assert_eq!(trace.provider_model, None);
}

#[test]
fn codex_jsonl_reports_usage_but_no_model_or_cost() {
    // Captured from codex-cli 0.144.1: successful JSONL contains
    // thread/turn events and turn.completed usage, but no model identity
    // or provider cost.
    let stdout = concat!(
        "{\"type\":\"thread.started\",\"thread_id\":\"thread-1\"}\n",
        "{\"type\":\"turn.started\"}\n",
        "{\"type\":\"item.completed\",\"item\":{\"id\":\"item-0\",\"type\":\"agent_message\",\"text\":\"{\\\"schemaVersion\\\":1,\\\"status\\\":\\\"success\\\",\\\"result\\\":{},\\\"error\\\":null}\"}}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":17389,\"cached_input_tokens\":2000,\"cache_write_tokens\":300,\"output_tokens\":22,\"reasoning_output_tokens\":0}}\n"
    );

    let (_, _, trace) =
        parse_and_validate_response(&exec(stdout, "", Some(0), true)).expect("Codex parses");
    assert_eq!(trace.usage.input, 17_389);
    assert_eq!(trace.usage.cache_read, 2_000);
    assert_eq!(trace.usage.cache_create, 300);
    assert_eq!(trace.usage.cache_create_1h, 0);
    assert_eq!(trace.usage.output, 22);
    assert_eq!(trace.provider_model, None);
    assert_eq!(trace.provider_cost_usd, None);
}

#[test]
fn grok_json_wrapper_reports_no_model_or_cost() {
    // Wrappers that still omit provider metadata (older Grok CLI, or a
    // result with only the text/stopReason envelope) leave both fields
    // unset. Live `grok` 1.0.5+ results add modelUsage/total_cost_usd and
    // are covered by grok_json_wrapper_extracts_model_usage_and_cost.
    let stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}",
        "stopReason": "EndTurn"
    })
    .to_string();

    let (_, _, trace) =
        parse_and_validate_response(&exec(&stdout, "", Some(0), true)).expect("Grok parses");
    assert_eq!(trace.provider_model, None);
    assert_eq!(trace.provider_cost_usd, None);
}

#[test]
fn grok_json_wrapper_extracts_model_usage_and_cost() {
    // Captured shape from Grok Build CLI 1.0.5 (`jrun-20260822-1925-3`):
    // `--model grok-4.6` produces a Claude-style wrapper whose usage
    // ledger key is `grok-4.6-build` and which reports total_cost_usd.
    // Extraction keeps the ledger key verbatim; ingest identity maps it
    // to the requested public menu id.
    let stdout = serde_json::json!({
        "text": "{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}",
        "stopReason": "EndTurn",
        "total_cost_usd": 0.0123,
        "modelUsage": {
            "grok-4.6-build": {
                "inputTokens": 100,
                "outputTokens": 20,
                "costUSD": 0.0123
            }
        }
    })
    .to_string();

    let (_, _, trace) =
        parse_and_validate_response(&exec(&stdout, "", Some(0), true)).expect("Grok parses");
    assert_eq!(trace.provider_model.as_deref(), Some("grok-4.6-build"));
    assert_eq!(trace.provider_cost_usd, Some(0.0123));
}

#[test]
fn synthesize_trace_falls_back_to_duration_only_when_stdout_unparseable() {
    // Plain non-JSON stdout: regression check that the previous "duration
    // only, zero usage" behavior is preserved when documents can't be
    // parsed at all.
    let trace = synthesize_trace(&exec("agent crashed", "stderr noise", Some(2), false));
    assert_eq!(trace.usage.input, 0);
    assert_eq!(trace.usage.output, 0);
    assert_eq!(trace.duration_ms, 1234);
}

#[test]
fn synthesize_trace_handles_empty_stdout() {
    // Empty stdout returns a parse error from serde; synthesize_trace must
    // still return a trace with duration set.
    let trace = synthesize_trace(&exec("", "boom", Some(1), false));
    assert_eq!(trace.usage.input, 0);
    assert_eq!(trace.usage.output, 0);
    assert_eq!(trace.duration_ms, 1234);
}

// ----- [ORB-10449] step-completion protocol check ---------------------

#[test]
fn protocol_check_rejects_prose_only_stdout() {
    // The stall shape: the provider exits 0 having emitted only prose, so
    // there is no termination signal at all.
    let stdout =
        r#"{"type":"result","subtype":"success","result":"still waiting on the background run"}"#;
    let error = response_envelope_protocol_check(stdout).expect_err("no envelope");
    assert!(
        error
            .to_string()
            .contains("does not contain an Orbit response envelope"),
        "{error}"
    );
}

#[test]
fn protocol_check_is_blind_to_declared_status() {
    // Frame only: every protocol status token satisfies the check, because
    // an agent that declares failure still ran its contract to the end.
    for status in ["success", "failed", "timeout"] {
        let stdout =
            format!(r#"{{"schemaVersion":1,"status":"{status}","result":{{}},"error":null}}"#);
        response_envelope_protocol_check(&stdout)
            .unwrap_or_else(|error| panic!("status {status} must satisfy the frame: {error}"));
    }
}

#[test]
fn protocol_check_rejects_an_unsupported_frame() {
    let unsupported_version = r#"{"schemaVersion":2,"status":"success","result":{},"error":null}"#;
    assert!(
        response_envelope_protocol_check(unsupported_version)
            .expect_err("bad version")
            .to_string()
            .contains("unsupported schemaVersion: 2")
    );

    let unknown_status = r#"{"schemaVersion":1,"status":"partial","result":{},"error":null}"#;
    assert!(
        response_envelope_protocol_check(unknown_status)
            .expect_err("bad status")
            .to_string()
            .contains("unknown status: partial")
    );
}

#[test]
fn protocol_check_finds_the_envelope_past_interleaved_non_json_stdout() {
    // A wrapped tool writing to the same stdout makes the document stream
    // unparseable, but the agent still terminated. Failing a completed step
    // over stray output would be worse than the defect this check catches.
    let stdout = concat!(
        "[main abc1234] chore: commit\n",
        " 1 file changed, 2 insertions(+)\n",
        r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#
    );
    response_envelope_protocol_check(stdout).expect("envelope after chatter");
}

#[test]
fn protocol_check_accepts_a_claude_wrapped_envelope() {
    // The healthy claude shape: the Orbit envelope arrives as a JSON string
    // nested in the wrapper's `result`.
    let inner = r#"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{},\"error\":null}"#;
    let stdout = format!(r#"{{"type":"result","subtype":"success","result":"{inner}"}}"#);
    response_envelope_protocol_check(&stdout).expect("wrapped envelope");
}

#[test]
fn peek_response_status_extracts_envelope_failed_from_claude_shaped_wrapper() {
    // Mimics the bug in T20260508-17: claude exits 0 with `result.subtype`
    // = "success" but the inner Orbit envelope (carried as a JSON-string
    // in `result`) reports `status: "failed"`. peek_response_status must
    // surface "failed" so the dispatcher can demote success without going
    // through validate_exit_alignment (which would reject the envelope
    // outright because exit==0 contradicts status=="failed").
    let inner = r#"{\"schemaVersion\":1,\"status\":\"failed\",\"error\":{\"code\":\"E\",\"message\":\"m\",\"details\":null}}"#;
    let stdout = format!(
        r#"{{"type":"result","subtype":"success","result":"{inner}","usage":{{"input_tokens":10,"output_tokens":3}}}}"#
    );
    assert_eq!(peek_response_status(&stdout).as_deref(), Some("failed"));
}

#[test]
fn peek_response_status_extracts_failed_from_prose_prefixed_claude_result() {
    let result = concat!(
        "I could not continue after the workspace disappeared.\n",
        r#"{"schemaVersion":1,"status":"failed","error":{"code":"workspace_unavailable","message":"worktree missing","details":null}}"#
    );
    let stdout = serde_json::json!({
        "type": "result",
        "subtype": "success",
        "result": result,
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3
        }
    })
    .to_string();

    assert_eq!(peek_response_status(&stdout).as_deref(), Some("failed"));
}

#[test]
fn peek_response_status_returns_none_when_no_envelope_present() {
    assert_eq!(peek_response_status("{\"hello\":\"world\"}"), None);
    assert_eq!(peek_response_status("{\"status\":\"failed\"}"), None);
    let prose_with_braces = serde_json::json!({
        "result": "prose with {arbitrary braces} and {\"status\":\"failed\"}, but no Orbit envelope"
    })
    .to_string();
    assert_eq!(peek_response_status(&prose_with_braces), None);
    assert_eq!(peek_response_status(""), None);
    assert_eq!(peek_response_status("not json"), None);
}

#[test]
fn peek_response_status_extracts_success_from_top_level_envelope() {
    let stdout = r#"{"schemaVersion":1,"status":"success","result":{}}"#;
    assert_eq!(peek_response_status(stdout).as_deref(), Some("success"));
}

#[test]
fn synthesize_response_failed_path_carries_usage() {
    // Empty stdout + non-zero exit triggers the synthesize "failed" path.
    // The trace returned alongside the synthesized envelope must preserve
    // usage when stdout is parseable, but here it's empty so usage stays
    // zero — verifies the synthesized envelope is wired to synthesize_trace.
    let exec = exec("", "agent crashed", Some(1), false);
    let (envelope, status, trace) = synthesize_response(&exec).expect("synthesized");
    assert_eq!(envelope.status, "failed");
    assert_eq!(status, AgentResponseStatus::Failed);
    assert_eq!(trace.duration_ms, 1234);
    assert_eq!(trace.usage.input, 0);
}

#[test]
fn grok_like_cli_response_extracts_nonzero_usage_and_tool_calls() {
    // Grok CLI --output-format json returns a wrapper with "text" containing
    // the Orbit envelope (plus any usage/tool metadata the CLI attaches).
    // The extraction must descend into "text" content to surface non-zero
    // token usage and tool invocations for diagnostics/metrics.
    let inner = r#"{"schemaVersion":1,"status":"success","result":{"pong":"grok"},"error":null,"usage":{"input_tokens":120,"output_tokens":35},"tool_calls":[{"id":"tc1","name":"orbit.task.show"}]}"#;
    let stdout = serde_json::json!({
        "text": inner,
        "stopReason": "EndTurn"
    })
    .to_string();
    let exec = exec(&stdout, "", Some(0), true);
    let (_, _, trace) = parse_and_validate_response(&exec).expect("grok-like parses");
    assert_eq!(trace.usage.input, 120);
    assert_eq!(trace.usage.output, 35);
    assert!(!trace.tool_calls.is_empty());
    assert_eq!(trace.tool_calls[0].tool_name, "orbit.task.show");
}
