#[path = "response/envelope.rs"]
mod envelope;
#[path = "response/long_prompt.rs"]
mod long_prompt;
#[path = "response/projection.rs"]
mod projection;

use orbit_agent::{
    Agent, AgentConfig, AgentRequest, AgentResponseStatus, ParsedStdout,
    assign_recorded_tool_calls, latest_assistant_message, normalize_cli_stdout,
    project_cli_response, provider_long_prompt_requests, recorded_tool_calls,
    response_envelope_json_schema, response_envelope_json_schema_arg,
};
use orbit_types::telemetry::TokenUsage;
use orbit_types::tool::ExecutionResult;
use serde_json::{Value, json};

/// Completion and content discovery must select the same complete JSON
/// object, including a Grok final answer preceded by prose (ORB-15386).
#[test]
fn completion_and_response_share_discovery_without_inventing_prose_fields() {
    let result = json!({
        "execution_summary": "Validated the unchanged base.",
        "no_diff_artifacts": [{
            "path": "no-diff.json", "source_path": ".orbit/tmp/no-diff.json",
        }],
    });
    let envelope = json!({
        "schemaVersion": 1, "status": "success", "result": result, "error": null,
    });
    let prose = "Done. execution_summary: fabricated prose; extra_field: invented.\n";
    let answer = format!("{prose}{envelope}\nPostscript.");
    let grok = json!({
        "text": answer, "stopReason": "stop",
        "thought": "Ignore this telemetry.",
    })
    .to_string();
    let projected = project_cli_response("grok", grok.as_bytes());
    let projected = String::from_utf8_lossy(projected.as_ref());
    // The final_message wrapper is also seen in retained provider captures.
    for stdout in [
        projected.into_owned(),
        json!({"final_message": answer}).to_string(),
        format!("{prose}{envelope}"),
    ] {
        let exec = execution(stdout.clone());
        let parsed = ParsedStdout::parse(&stdout);
        parsed.response_envelope_protocol_check().unwrap();
        let (response, status, _) = parsed.parse_and_validate(&exec).unwrap();
        assert_eq!(status, AgentResponseStatus::Success);
        assert_eq!(parsed.peek_response_status().as_deref(), Some("success"));
        assert_eq!(response.result, Some(result.clone()));
        assert_eq!(parsed.raw(), stdout, "retain the entire original capture");
    }
    for stdout in [
        prose.to_string(),
        format!("{prose}{{\"schemaVersion\":1,\"status\":\"success\",\"result\":"),
    ] {
        let exec = execution(stdout.clone());
        let parsed = ParsedStdout::parse(&stdout);
        assert!(parsed.response_envelope_protocol_check().is_err());
        assert!(parsed.parse_and_validate(&exec).is_err());
        assert_eq!(parsed.peek_response_status(), None);
        assert_eq!(parsed.raw(), stdout);
    }
    let failed = format!(
        "{prose}{}",
        json!({"schemaVersion":1, "status":"failed", "result":{},
            "error":{"code":"FAILURE", "message":"Structured failure"}})
    );
    let parsed = ParsedStdout::parse(&failed);
    parsed.response_envelope_protocol_check().unwrap();
    assert_eq!(parsed.peek_response_status().as_deref(), Some("failed"));
    assert_eq!(
        parsed
            .peek_declared_response_failure()
            .unwrap()
            .error
            .unwrap()
            .code,
        "FAILURE",
    );
    let mut exec = execution(failed);
    exec.exit_code = Some(1);
    let (_, status, _) = ParsedStdout::parse(&exec.stdout)
        .parse_and_validate(&exec)
        .unwrap();
    assert_eq!(status, AgentResponseStatus::Failed);
}

#[test]
fn terminal_response_selection_is_independent_of_chatter() {
    let success = json!({
        "schemaVersion": 1, "status": "success", "result": {"source": "success"},
        "error": null,
    });
    for (token, expected) in [
        ("failed", AgentResponseStatus::Failed),
        ("timeout", AgentResponseStatus::Timeout),
    ] {
        let failure = json!({
            "schemaVersion": 1, "status": token, "result": {"source": "failure"},
            "error": {"code": "FINAL_FAILURE", "message": "work failed", "details": null},
        });
        for (earlier, terminal, status) in [
            (&success, &failure, expected),
            (&failure, &success, AgentResponseStatus::Success),
        ] {
            for answer in [
                format!("{earlier}\n{terminal}"),
                format!("Note:\n{earlier}\n{terminal}"),
                format!("Note:\n{earlier}\nWarning.\n{terminal}\nPostscript."),
            ] {
                for stdout in answer_captures(&answer) {
                    let parsed = ParsedStdout::parse(&stdout);
                    parsed.response_envelope_protocol_check().unwrap();
                    assert_eq!(
                        parsed.peek_response_status().as_deref(),
                        terminal["status"].as_str(),
                    );
                    let declared = parsed.peek_declared_response_failure();
                    if status == AgentResponseStatus::Success {
                        assert_eq!(declared, None);
                    } else {
                        let declared = declared.expect("terminal failure stays authoritative");
                        assert_eq!(declared.status, token);
                        let error = declared.error.expect("preserve the declared error");
                        assert_eq!(error.code, "FINAL_FAILURE");
                        assert_eq!(error.message, "work failed");
                        assert!(
                            parsed
                                .parse_and_validate(&execution(stdout.clone()))
                                .is_err(),
                            "an exit-0 terminal failure must not return stale successful output",
                        );
                    }

                    let mut exec = execution(stdout.clone());
                    exec.success = status == AgentResponseStatus::Success;
                    exec.exit_code = Some(if exec.success { 0 } else { 1 });
                    exec.timed_out = status == AgentResponseStatus::Timeout;
                    let (envelope, selected_status, _) = parsed.parse_and_validate(&exec).unwrap();
                    assert_eq!(selected_status, status);
                    assert_eq!(envelope.result.as_ref(), Some(&terminal["result"]));
                    assert_eq!(
                        serde_json::to_value(envelope.error).unwrap(),
                        terminal["error"],
                    );
                }
            }
        }
    }
}

#[test]
fn terminal_protocol_validation_does_not_fall_back_to_earlier_success() {
    let earlier = json!({"schemaVersion": 1, "status": "success", "result": {}});
    for terminal in [
        json!({"schemaVersion": 2, "status": "success", "result": {}}),
        json!({"schemaVersion": 1, "status": "unknown", "result": {}}),
        json!({"schemaVersion": "1", "status": "success", "result": {}}),
        json!({"schemaVersion": null, "status": "success", "result": {}}),
        json!({"schemaVersion": 1, "status": 17, "result": {}}),
    ] {
        for prefix in ["", "Note:\n"] {
            for stdout in answer_captures(&format!("{prefix}{earlier}\n{terminal}")) {
                let parsed = ParsedStdout::parse(&stdout);
                assert!(parsed.response_envelope_protocol_check().is_err());
                assert!(
                    parsed
                        .parse_and_validate(&execution(stdout.clone()))
                        .is_err()
                );
                assert_eq!(
                    parsed.peek_response_status(),
                    terminal["status"].as_str().map(str::to_owned)
                );
                assert_eq!(parsed.peek_declared_response_failure(), None);
            }
        }
    }
}

#[test]
fn malformed_terminal_payloads_cannot_return_an_earlier_success() {
    let earlier = json!({
        "schemaVersion": 1, "status": "success", "result": {"source": "earlier"},
        "error": null,
    });
    for token in ["success", "failed", "timeout"] {
        for (error, duration, declared_error) in [
            (
                json!({"code": 17, "message": "terminal failure"}),
                json!(10),
                false,
            ),
            (json!({"code": "FINAL_FAILURE"}), json!(10), false),
            (
                json!({"code": "FINAL_FAILURE", "message": false}),
                json!(10),
                false,
            ),
            (json!("terminal failure"), json!(10), false),
            (json!([earlier]), json!(10), false),
            (
                json!({"code": "FINAL_FAILURE", "message": "terminal failure"}),
                json!("10"),
                true,
            ),
        ] {
            let terminal = json!({
                "schemaVersion": 1, "status": token, "result": {"quoted": earlier},
                "error": error, "durationMs": duration,
            });
            let mut captures = Vec::new();
            for prefix in ["", "Note:\n"] {
                captures.extend(answer_captures(&format!("{prefix}{earlier}\n{terminal}")));
            }
            // A selected malformed frame also prevents wrapper diagnostics
            // from synthesizing an outcome when no earlier envelope exists.
            captures.push(
                json!({
                    "structured_output": terminal, "result": earlier,
                    "is_error": true, "subtype": "error_max_turns",
                })
                .to_string(),
            );
            captures.push(
                json!({
                    "structured_output": terminal,
                    "is_error": true, "subtype": "error_max_turns",
                })
                .to_string(),
            );
            for stdout in captures {
                let parsed = ParsedStdout::parse(&stdout);
                // Completion validates the frame, not its payload types.
                parsed.response_envelope_protocol_check().unwrap();
                assert_eq!(parsed.peek_response_status().as_deref(), Some(token));
                let failure = parsed.peek_declared_response_failure();
                if token == "success" {
                    assert_eq!(failure, None);
                } else {
                    let failure =
                        failure.expect("keep the terminal failure despite invalid fields");
                    assert_eq!(failure.status, token);
                    if declared_error {
                        let error = failure.error.expect("retain well-typed error diagnostics");
                        assert_eq!(error.code, "FINAL_FAILURE");
                        assert_eq!(error.message, "terminal failure");
                    } else {
                        assert_eq!(failure.error, None);
                    }
                }
                let error = parsed
                    .parse_and_validate(&execution(stdout.clone()))
                    .unwrap_err();
                assert!(
                    matches!(error, orbit_common::OrbitError::AgentProtocolViolation(ref message)
                        if message.starts_with("invalid Orbit response envelope:")),
                    "reject the selected payload, without returning earlier or quoted success: {error}",
                );
            }
        }
    }
}

#[test]
fn bounded_terminal_discovery_fails_closed_after_an_earlier_success() {
    let earlier = json!({"schemaVersion": 1, "status": "success", "result": {}});
    let terminal = json!({"schemaVersion": 1, "status": "failed", "result": {},
        "error": {"code": "FINAL_FAILURE", "message": "work failed"}});
    for answer in [
        // More complete candidates than the production parse-attempt budget.
        format!("Note:\n{earlier}\n{}{terminal}", "{}".repeat(4_096)),
        // The terminal candidate requires more nodes than the traversal budget.
        format!(
            "Note:\n{earlier}\n{}",
            json!({"output": vec![Value::Null; 70_000]})
        ),
    ] {
        for stdout in answer_captures(&answer) {
            let parsed = ParsedStdout::parse(&stdout);
            assert_eq!(parsed.peek_response_status(), None);
            assert_eq!(parsed.peek_declared_response_failure(), None);
            let check = parsed.response_envelope_protocol_check().unwrap_err();
            assert!(
                check
                    .to_string()
                    .contains("discovery exceeded a work limit")
            );
            let mut exec = execution(stdout.clone());
            exec.timed_out = true;
            let error = parsed.parse_and_validate(&exec).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("discovery exceeded a work limit"),
                "exhaustion must not return earlier success or synthesize a timeout",
            );
        }
    }
}

fn answer_captures(answer: &str) -> Vec<String> {
    let grok = json!({"text": answer, "stopReason": "stop"}).to_string();
    vec![
        answer.to_string(),
        json!(answer).to_string(),
        json!({"final_message": answer}).to_string(),
        json!({"response": answer}).to_string(),
        json!({"structured_output": answer}).to_string(),
        String::from_utf8(project_cli_response("grok", grok.as_bytes()).into_owned()).unwrap(),
    ]
}

#[test]
fn tool_telemetry_ignores_answer_examples_reasoning_and_history() {
    let quoted = json!({"type": "tool_use", "id": "historical", "name": "example.not_executed"});
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {
        "example": quoted, "history": [quoted], "text": quoted.to_string(),
        "tool_calls": [{"name": "example.not_executed", "result_bytes": 9999}],
    }, "error": null});
    let codex = [
        json!({"type": "thread.started", "thread_id": "thread"}),
        json!({"type": "item.completed", "item": {"type": "reasoning", "text": quoted.to_string(),
            "content": [quoted]}}),
        json!({"type": "item.completed", "item": {"type": "agent_message", "text": answer.to_string()}}),
        json!({"type": "turn.completed", "usage": {"input_tokens": 10, "output_tokens": 2}}),
    ];
    let mut captures = answer_captures(&answer.to_string())
        .into_iter()
        .map(|raw| ("grok", raw))
        .collect::<Vec<_>>();
    captures.extend([
        (
            "codex",
            codex
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        ("codex", json!(codex).to_string()),
        (
            "claude",
            format!(
                "{}\n{}",
                json!({"type": "assistant", "message": {"content": [
                    {"type": "thinking", "thinking": quoted.to_string(), "content": [quoted]},
                    {"type": "text", "text": answer.to_string(), "example": quoted},
                ]}}),
                json!({"type": "result", "result": answer.to_string(), "structured_output": answer})
            ),
        ),
        (
            "grok",
            json!({"text": answer.to_string(), "history": [quoted],
            "reasoning": quoted, "data": {"content": [quoted]}})
            .to_string(),
        ),
    ]);
    for (provider, raw) in captures {
        let normalized = normalize_cli_stdout(provider, raw.as_bytes());
        let stdout = String::from_utf8(normalized.into_owned()).unwrap();
        let (response, status, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        assert_eq!(status, AgentResponseStatus::Success);
        assert_eq!(response.result, Some(answer["result"].clone()));
        assert!(
            trace.tool_calls.is_empty(),
            "{provider}: {raw}: {:?}",
            trace.tool_calls
        );
        if provider == "codex" {
            assert_eq!(trace.usage.input, 10);
            assert_eq!(trace.usage.output, 2);
        }
    }
}

#[test]
fn codex_tool_telemetry_correlates_ids_and_keeps_payloads_opaque() {
    let quoted = json!({"type": "tool_result", "tool_use_id": "command", "name": "fake",
        "result_bytes": 9999, "tool_calls": [{"name": "fake"}]});
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {
        "example": quoted, "history": [{"type": "tool_use", "name": "fake"}],
    }, "error": null});
    let output = json!(quoted.to_string());
    let raw = [
        json!({"type": "item.started", "item": {"type": "command_execution", "id": "command"}}),
        json!({"type": "item.started", "item": {"type": "mcp_tool_call", "id": "mcp", "name": "fixture.read"}}),
        json!({"type": "item.updated", "item": {"type": "command_execution", "id": "command", "aggregated_output": "partial"}}),
        // Completions arrive in request order, not the collector's latest-open order.
        json!({"type": "item.completed", "item": {"type": "command_execution", "id": "command", "aggregated_output": output}}),
        json!({"type": "item.completed", "item": {"type": "mcp_tool_call", "id": "mcp", "name": "fixture.read", "result": {"ok": true}}}),
        // A completed-only event must not overwrite another request with a different ID.
        json!({"type": "item.started", "item": {"type": "command_execution", "id": "pending"}}),
        json!({"type": "item.completed", "item": {"type": "command_execution", "id": "standalone", "aggregated_output": "done"}}),
        json!({"type": "item.completed", "item": {"type": "agent_message", "text": answer.to_string()}}),
        json!({"type": "turn.completed"}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let normalized = normalize_cli_stdout("codex", raw.as_bytes());
    let stdout = String::from_utf8(normalized.into_owned()).unwrap();
    let (_, _, trace) = ParsedStdout::parse(&stdout)
        .parse_and_validate(&execution(stdout.clone()))
        .unwrap();
    let actual = trace
        .tool_calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                1,
                "command_execution",
                serde_json::to_vec(&output).unwrap().len() as u64
            ),
            (
                2,
                "fixture.read",
                serde_json::to_vec(&json!({"ok": true})).unwrap().len() as u64
            ),
            (3, "command_execution", 0),
            (
                4,
                "command_execution",
                serde_json::to_vec(&json!("done")).unwrap().len() as u64
            ),
        ]
    );
}

#[test]
fn codex_file_change_telemetry_dedupes_by_item_id_and_keeps_order() {
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null});
    // Observed Codex shape (ORB-20043): item_13 file_change started then
    // completed with 3 changes; patch contents are not retained.
    let patch = "SECRET_PATCH_CONTENT";
    let raw = [
        json!({"type": "item.started", "item": {"type": "command_execution", "id": "read-1"}}),
        json!({"type": "item.completed", "item": {"type": "command_execution", "id": "read-1", "aggregated_output": "alpha"}}),
        json!({"type": "item.started", "item": {"type": "mcp_tool_call", "id": "read-2", "name": "fixture.read"}}),
        json!({"type": "item.completed", "item": {"type": "mcp_tool_call", "id": "read-2", "name": "fixture.read", "result": {"ok": true}}}),
        json!({"type": "item.started", "item": {"type": "file_change", "id": "item_13", "status": "in_progress", "changes": [{"path": "a.rs", "kind": {"type": "update"}, "diff": patch}]}}),
        json!({"type": "item.completed", "item": {"type": "file_change", "id": "item_13", "status": "completed", "changes": [{"path": "a.rs", "kind": {"type": "update"}, "diff": patch}, {"path": "b.rs", "kind": {"type": "add"}, "diff": patch}, {"path": "c.rs", "kind": {"type": "delete"}, "diff": patch}]}}),
        // A second distinct item identity is a second edit call.
        json!({"type": "item.started", "item": {"type": "file_change", "id": "item_14"}}),
        json!({"type": "item.completed", "item": {"type": "file_change", "id": "item_14", "status": "completed", "changes": []}}),
        // Bare top-level and assistant-content file_change objects are not
        // provider item frames and must not be counted.
        json!({"type": "file_change", "id": "bare", "changes": []}),
        json!({"type": "assistant", "message": {"content": [{"type": "file_change", "id": "in-message"}]}}),
        // JSON nested inside an agent message remains prose, not a frame.
        json!({"type": "item.completed", "item": {"type": "agent_message", "text": json!({"type": "item.completed", "item": {"type": "file_change", "id": "nested"}}).to_string()}}),
        json!({"type": "item.completed", "item": {"type": "agent_message", "text": answer.to_string()}}),
        json!({"type": "turn.completed"}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let normalized = normalize_cli_stdout("codex", raw.as_bytes());
    let stdout = String::from_utf8(normalized.into_owned()).unwrap();
    let (_, _, mut trace) = ParsedStdout::parse(&stdout)
        .parse_and_validate(&execution(stdout.clone()))
        .unwrap();
    // Codex file_change is already on the normalized stdout. The provider
    // overlay must leave those calls in place.
    assign_recorded_tool_calls("codex", raw.as_bytes(), &mut trace);
    let actual = trace
        .tool_calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                1,
                "command_execution",
                serde_json::to_vec(&json!("alpha")).unwrap().len() as u64
            ),
            (
                2,
                "fixture.read",
                serde_json::to_vec(&json!({"ok": true})).unwrap().len() as u64
            ),
            (3, "file_change", 0),
            (4, "file_change", 0),
        ]
    );
    // No patch payload is retained in the trace.
    assert!(trace.tool_calls.iter().all(|c| c.result_payload.is_none()));
    assert!(!format!("{:?}", trace.tool_calls).contains(patch));
}

#[test]
fn tool_telemetry_preserves_direct_message_and_inline_contracts() {
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null});
    let payload = json!({"type": "tool_use", "name": "quoted.not_executed"});
    for events in [
        vec![
            json!({"type": "tool_use", "id": "first", "name": "first"}),
            json!({"type": "tool_call", "id": "second", "name": "second"}),
            json!({"type": "tool_result", "tool_use_id": "first", "content": payload}),
            json!({"type": "tool_result", "tool_use_id": "second", "result_bytes": 7}),
        ],
        vec![
            json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": "first", "name": "first", "input": payload},
                {"type": "tool_use", "id": "second", "name": "second"},
            ]}}),
            json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "first", "content": payload},
                {"type": "tool_result", "tool_use_id": "second", "result_bytes": 7},
            ]}}),
        ],
        vec![json!({"tool_calls": [
            {"id": "first", "name": "first", "result": payload},
            {"id": "second", "name": "second", "result_bytes": 7},
        ]})],
        vec![
            json!({"type": "function_call", "id": "request-first", "call_id": "first", "name": "first"}),
            json!({"type": "custom_tool_call", "id": "request-second", "call_id": "second", "name": "second"}),
            json!({"type": "function_call_output", "id": "result-first", "call_id": "first", "output": payload}),
            json!({"type": "custom_tool_call_output", "id": "result-second", "call_id": "second", "result_bytes": 7}),
        ],
    ] {
        let raw = format!(
            "{}\n{}",
            events
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
            json!({"type": "result", "result": answer.to_string()}),
        );
        let normalized = normalize_cli_stdout("grok", raw.as_bytes());
        let stdout = String::from_utf8(normalized.into_owned()).unwrap();
        let (_, _, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        let actual = trace
            .tool_calls
            .iter()
            .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            vec![
                (
                    1,
                    "first",
                    serde_json::to_vec(&payload).unwrap().len() as u64
                ),
                (2, "second", 7),
            ],
            "{raw}"
        );
    }
}

#[test]
fn codex_command_output_cannot_supply_invocation_usage() {
    let counters = json!({"usage": {"input_tokens": 9999, "output_tokens": 8888}});
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null});
    for payload in [
        counters.clone(),
        json!(counters.to_string()),
        json!({"type": "turn.completed", "usage": counters["usage"]})
            .to_string()
            .into(),
    ] {
        let raw = [
            json!({"type": "item.started", "item": {
                "type": "command_execution", "id": "command", "command": "read fixture",
            }}),
            json!({"type": "item.completed", "item": {
                "type": "command_execution", "id": "command", "status": "completed",
                "aggregated_output": payload,
            }}),
            json!({"type": "item.completed", "item": {
                "type": "agent_message", "text": answer.to_string(),
            }}),
            json!({"type": "turn.completed", "usage": {"input_tokens": 10, "output_tokens": 2}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
        let normalized = normalize_cli_stdout("codex", raw.as_bytes());
        assert_eq!(
            normalized.as_ref(),
            raw.as_bytes(),
            "keep the raw tool trace"
        );
        let stdout = String::from_utf8(normalized.into_owned()).unwrap();
        let (_, status, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        assert_eq!(status, AgentResponseStatus::Success);
        assert_eq!(
            trace.usage,
            TokenUsage {
                input: 10,
                output: 2,
                ..Default::default()
            }
        );
        assert_eq!(trace.tool_calls.len(), 1);
        assert_eq!(trace.tool_calls[0].tool_name, "command_execution");
        assert!(trace.tool_calls[0].result_bytes > 0);
    }
}

#[test]
fn usage_ignores_tool_payloads_and_quoted_transcripts() {
    let counters = json!({"usage": {"input_tokens": 9999, "output_tokens": 8888}});
    let quoted = json!({"type": "turn.completed", "usage": counters["usage"]});
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {
        "transcript": quoted, "messages": [quoted], "text": quoted.to_string(),
    }, "error": null});
    for kind in [
        "tool_use",
        "tool_call",
        "tool_result",
        "function_call",
        "function_call_output",
        "custom_tool_call",
        "custom_tool_call_output",
        "mcp_tool_call",
        "command_execution",
        "user",
        "reasoning",
        "thinking",
    ] {
        let tool = json!({"type": kind, "input_tokens": 9999, "output_tokens": 8888,
            "usage": counters["usage"], "input": counters, "arguments": counters.to_string(),
            "result": counters, "output": counters.to_string(), "content": quoted,
        });
        let raw = format!(
            "{tool}\n{}",
            json!({
                "text": answer.to_string(), "thought": quoted.to_string(), "history": [quoted],
                "tool_calls": [{"function": {"arguments": counters.to_string()}}],
                "usage": {"input_tokens": 10, "output_tokens": 2},
            })
        );
        let normalized = normalize_cli_stdout("grok", raw.as_bytes());
        let stdout = String::from_utf8(normalized.into_owned()).unwrap();
        let (_, _, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        assert_eq!(
            trace.usage,
            TokenUsage {
                input: 10,
                output: 2,
                ..Default::default()
            },
            "{kind}"
        );
    }
    let raw = [
        json!({"type": "assistant.message", "data": {"content": answer.to_string(),
            "toolRequests": [{"arguments": counters}], "transcript": quoted}}),
        json!({"type": "assistant.usage", "data": {"input_tokens": 10, "output_tokens": 2}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let normalized = normalize_cli_stdout("copilot", raw.as_bytes());
    let stdout = String::from_utf8(normalized.into_owned()).unwrap();
    let (_, _, trace) = ParsedStdout::parse(&stdout)
        .parse_and_validate(&execution(stdout.clone()))
        .unwrap();
    assert_eq!(
        trace.usage,
        TokenUsage {
            input: 10,
            output: 2,
            ..Default::default()
        }
    );
}

#[test]
fn declared_answer_usage_and_provider_model_rollups_survive() {
    let answer = json!({"schemaVersion": 1, "status": "success", "error": null,
        "result": {"usage": {"input_tokens": 10, "output_tokens": 2},
            "transcript": {"usage": {"input_tokens": 9999}}}});
    for stdout in answer_captures(&answer.to_string()) {
        let (_, _, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        assert_eq!(
            trace.usage,
            TokenUsage {
                input: 10,
                output: 2,
                ..Default::default()
            }
        );
    }
    for key in [
        "usage",
        "token_usage",
        "tokenUsage",
        "usageMetadata",
        "usage_metadata",
    ] {
        let mut answer =
            json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null});
        answer[key] = json!({"input_tokens": "10", "output_tokens": "2"});
        let stdout = answer.to_string();
        let (_, _, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        assert_eq!(
            trace.usage,
            TokenUsage {
                input: 10,
                output: 2,
                ..Default::default()
            },
            "{key}"
        );
    }
    for (provider, metadata, expected) in [
        (
            "grok",
            json!({"modelUsage": {"model": {"inputTokens": 10, "outputTokens": 2}}}),
            TokenUsage {
                input: 10,
                output: 2,
                ..Default::default()
            },
        ),
        (
            "gemini",
            json!({"stats": {"models": {"model": {
                "tokens": {"input": 10, "candidates": 2, "thoughts": 3, "tool": 1, "cached": 4},
                "roles": {"assistant": {"tokens": {"input": 10, "candidates": 2, "thoughts": 3, "tool": 1, "cached": 4}}},
            }}}}),
            TokenUsage {
                input: 10,
                output: 6,
                cache_read: 4,
                ..Default::default()
            },
        ),
    ] {
        let mut wrapper = metadata;
        wrapper["response"] =
            json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null})
                .to_string()
                .into();
        let raw = wrapper.to_string();
        let normalized = normalize_cli_stdout(provider, raw.as_bytes());
        let stdout = String::from_utf8(normalized.into_owned()).unwrap();
        let (_, _, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        assert_eq!(trace.usage, expected, "{provider}");
    }
}

#[test]
fn provider_usage_precedes_declared_answer_fallback() {
    let counters = json!({"input_tokens":10, "output_tokens":2,
        "cache_read_input_tokens":5, "cache_creation_input_tokens":7,
        "cache_creation":{"ephemeral_5m_input_tokens":3, "ephemeral_1h_input_tokens":4}});
    let expected = TokenUsage {
        input: 10,
        output: 2,
        cache_read: 5,
        cache_create: 3,
        cache_create_1h: 4,
    };
    let mut answer = json!({"schemaVersion":1, "status":"success", "error":null,
        "result":{"usage":counters, "transcript":{"usage":{"input_tokens":9999}}}});
    for top_level in [false, true] {
        if top_level {
            answer["usage"] = counters.clone();
        }
        for provider in ["claude", "grok", "antigravity", "gemini", "codex"] {
            for has_provider_usage in [true, false] {
                let mut wrapper = match provider {
                    "claude" => {
                        json!({"type":"result", "result":answer.to_string(), "structured_output":answer})
                    }
                    "grok" => json!({"text":answer.to_string(), "stopReason":"end_turn"}),
                    "antigravity" => json!({"status":"SUCCESS", "response":answer.to_string()}),
                    "gemini" => json!({"response":answer.to_string()}),
                    "codex" => {
                        json!({"type":"item.completed", "item":{"type":"agent_message", "text":answer.to_string()}})
                    }
                    _ => unreachable!(),
                };
                if has_provider_usage && provider != "codex" {
                    if provider == "gemini" {
                        wrapper["stats"] = json!({"models":{"model":{"usage":counters}}});
                    } else {
                        wrapper["usage"] = counters.clone();
                    }
                }
                let mut raw = wrapper.to_string();
                if provider == "codex" && has_provider_usage {
                    raw.push_str(&format!(
                        "\n{}",
                        json!({"type":"turn.completed", "usage":counters})
                    ));
                }
                let stdout =
                    String::from_utf8(normalize_cli_stdout(provider, raw.as_bytes()).into_owned())
                        .unwrap();
                let (_, _, trace) = ParsedStdout::parse(&stdout)
                    .parse_and_validate(&execution(stdout.clone()))
                    .unwrap();
                assert_eq!(
                    trace.usage, expected,
                    "{provider}, provider counters={has_provider_usage}, envelope counters={top_level}"
                );
            }
        }
    }
}

#[test]
fn claude_terminal_rollup_preserves_cache_ttl_without_counting_quotes() {
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {
        "transcript": {"usage": {"input_tokens": 9999, "output_tokens": 8888}},
    }, "error": null});
    for (split, expected_cache) in [
        (
            json!({"ephemeral_5m_input_tokens": 3, "ephemeral_1h_input_tokens": 4}),
            (3, 4),
        ),
        (json!({"ephemeral_1h_input_tokens": 7}), (0, 7)),
        (Value::Null, (7, 0)),
    ] {
        let raw = format!(
            "{}\n{}",
            json!({"type": "assistant", "message": {
                "usage": {"input_tokens": 9999, "output_tokens": 8888},
            }}),
            json!({"type": "result", "result": answer.to_string(), "structured_output": answer,
                "usage": {"input_tokens": 10, "output_tokens": 2, "cache_read_input_tokens": 5,
                    "cache_creation_input_tokens": 7, "cache_creation": split},
                "modelUsage": {"model": {"inputTokens": 10, "outputTokens": 2,
                    "cacheReadInputTokens": 5, "cacheCreationInputTokens": 7}},
            })
        );
        let normalized = normalize_cli_stdout("claude", raw.as_bytes());
        let stdout = String::from_utf8(normalized.into_owned()).unwrap();
        let (_, _, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .unwrap();
        assert_eq!(
            trace.usage,
            TokenUsage {
                input: 10,
                output: 2,
                cache_read: 5,
                cache_create: expected_cache.0,
                cache_create_1h: expected_cache.1
            }
        );
    }
}

// [ORB-19680] Prompt-size price tiers bill one request by its own prompt, and
// Claude's `result` reports only the run's totals. Legacy streams without
// partial message events have only starting output counts, so a possibly-long
// request leaves per-request usage unknown, never underpriced.
#[test]
fn claude_legacy_stream_proves_only_that_no_request_reached_the_tier() {
    let message = |model: &str, cache_read: u64| {
        json!({"type": "assistant", "message": {
            "id": format!("msg_{cache_read}"), "model": model,
            "usage": {"input_tokens": 3, "cache_read_input_tokens": cache_read,
                      "cache_creation_input_tokens": 2_000, "output_tokens": 7},
        }})
        .to_string()
    };
    let result = json!({"type": "result", "usage": {"input_tokens": 9}}).to_string();
    let stream = |messages: &[String]| {
        let mut frames = vec![json!({"type": "system", "subtype": "init"}).to_string()];
        frames.extend_from_slice(messages);
        frames.push(result.clone());
        frames.join("\n")
    };
    let short = stream(&[
        message("claude-haiku-5-5", 40_000),
        message("claude-haiku-5-5", 90_000),
        // A subagent on another model is not this invocation's request.
        message("claude-opus-5-5", 300_000),
    ]);
    let long = stream(&[
        message("claude-haiku-5-5", 40_000),
        message("claude-haiku-5-5", 120_000),
    ]);
    let now = chrono::Utc::now();
    let haiku = Some("claude-haiku-5-5[1m]");

    assert_eq!(
        provider_long_prompt_requests("claude", short.as_bytes(), true, None, haiku, now),
        Some(Vec::new()),
        "every request on the invocation's model stayed under the floor"
    );
    assert_eq!(
        provider_long_prompt_requests("claude", long.as_bytes(), true, None, haiku, now),
        None,
        "a long request's final output is not on the stream"
    );
    assert_eq!(
        provider_long_prompt_requests("claude", short.as_bytes(), false, None, haiku, now),
        None,
        "a truncated capture may have lost requests"
    );
    assert_eq!(
        provider_long_prompt_requests("claude", result.as_bytes(), true, None, None, now),
        None,
        "a single `--output-format json` document has no per-request usage"
    );
}

// [ORB-19680] Codex's stdout totals only the turn; its session rollout records
// each request's `last_token_usage`. A repeated `token_count` whose running
// total did not move is no new request.
#[test]
fn codex_rollout_keeps_each_long_prompt_request() {
    let codex_home = crate::support::scratch();
    let now = chrono::Utc::now();
    let day = now.with_timezone(&chrono::Local).date_naive();
    let sessions = codex_home
        .path()
        .join("sessions")
        .join(day.format("%Y").to_string())
        .join(day.format("%m").to_string())
        .join(day.format("%d").to_string());
    std::fs::create_dir_all(&sessions).unwrap();
    let token_count = |total: u64, input: u64, cached: u64| {
        json!({"type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"total_tokens": total},
            "last_token_usage": {"input_tokens": input, "cached_input_tokens": cached,
                                 "output_tokens": 900, "total_tokens": input + 900},
        }}})
        .to_string()
    };
    let rollout = [
        json!({"type": "session_meta", "payload": {"id": "thread-a"}}).to_string(),
        json!({"type":"event_msg", "payload":{"type":"task_started", "turn_id":"original"}})
            .to_string(),
        token_count(300_900, 300_000, 280_000),
        // A rate-limit refresh repeats the last request.
        token_count(300_900, 300_000, 280_000),
        token_count(351_800, 50_000, 40_000),
    ]
    .join("\n");
    std::fs::write(
        sessions.join("rollout-2026-10-29T04-00-00-thread-a.jsonl"),
        &rollout,
    )
    .unwrap();
    let stdout = format!(
        "{}\n{}",
        json!({"type": "thread.started", "thread_id": "thread-a"}),
        json!({"type":"turn.started"})
    );

    assert_eq!(
        provider_long_prompt_requests(
            "codex",
            stdout.as_bytes(),
            false,
            Some(codex_home.path()),
            None,
            now
        ),
        Some(vec![TokenUsage {
            input: 300_000,
            cache_read: 280_000,
            output: 900,
            ..TokenUsage::default()
        }])
    );
    let path = sessions.join("rollout-2026-10-29T04-00-00-thread-a.jsonl");
    let mut resumed = rollout.clone();
    for (turn, total, input, cached, expected) in [
        ("short", 352_700, 0, 0, vec![]),
        (
            "long",
            653_600,
            300_000,
            280_000,
            vec![TokenUsage {
                input: 300_000,
                cache_read: 280_000,
                output: 900,
                ..Default::default()
            }],
        ),
        ("short-again", 654_500, 0, 0, vec![]),
    ] {
        resumed.push_str(&format!(
            "\n{}\n{}\n{}",
            json!({"type":"event_msg", "payload":{"type":"task_started", "turn_id":turn}}),
            // A refresh of the previous turn is not a new request.
            token_count(total - input - 900, 300_000, 280_000),
            token_count(total, input, cached)
        ));
        std::fs::write(&path, &resumed).unwrap();
        assert_eq!(
            provider_long_prompt_requests(
                "codex",
                stdout.as_bytes(),
                false,
                Some(codex_home.path()),
                None,
                now
            ),
            Some(expected),
            "resume {turn}"
        );
    }
    std::fs::write(&path, token_count(300_900, 300_000, 280_000)).unwrap();
    assert_eq!(
        provider_long_prompt_requests(
            "codex",
            stdout.as_bytes(),
            true,
            Some(codex_home.path()),
            None,
            now
        ),
        None,
        "missing turn boundary is unknown"
    );
    assert_eq!(
        provider_long_prompt_requests("grok", b"{}", true, None, None, now),
        None,
        "Grok reports invocation totals only"
    );
}

/// Trace from the reduced stdout, then the tool calls the raw capture recorded.
fn traced(provider: &str, raw: &str) -> orbit_types::telemetry::InvocationTrace {
    let normalized = normalize_cli_stdout(provider, raw.as_bytes());
    let stdout = String::from_utf8(normalized.into_owned()).unwrap();
    let (_, _, mut trace) = ParsedStdout::parse(&stdout)
        .parse_and_validate(&execution(stdout.clone()))
        .unwrap();
    assign_recorded_tool_calls(provider, raw.as_bytes(), &mut trace);
    trace
}

fn reduced_tool_names(provider: &str, raw: &str) -> Vec<String> {
    let normalized = normalize_cli_stdout(provider, raw.as_bytes());
    let stdout = String::from_utf8(normalized.into_owned()).unwrap();
    let (_, _, trace) = ParsedStdout::parse(&stdout)
        .parse_and_validate(&execution(stdout.clone()))
        .unwrap();
    trace
        .tool_calls
        .into_iter()
        .map(|call| call.tool_name)
        .collect()
}

// [ORB-20534] Claude's answer reduction keeps only the terminal result, so a
// stored trace had no Edit even though the stream recorded one. Usage stays
// the result's; assistant messages repeat a request's starting usage.
#[test]
fn claude_stream_edit_is_recorded_without_summing_assistant_usage() {
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null});
    let patch = "CLAUDE_PATCH_SECRET";
    let assistant = json!({"type": "assistant", "message": {
        "usage": {"input_tokens": 9999, "output_tokens": 8888},
        "content": [
            {"type": "tool_use", "id": "bash-1", "name": "Bash", "input": {"command": "ls"}},
            {"type": "tool_use", "id": "edit-1", "name": "Edit", "input": {"old_string": patch}},
        ],
    }});
    let user = json!({"type": "user", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "bash-1", "content": "listed"},
        {"type": "tool_result", "tool_use_id": "edit-1", "content": "patched"},
    ]}});
    let result = json!({"type": "result", "result": answer.to_string(), "usage": {
        "input_tokens": 10, "output_tokens": 2,
    }});
    let result_line = result.to_string();
    let raw = format!("{assistant}\n{user}\n{result_line}");

    let projected = project_cli_response("claude", raw.as_bytes());
    assert_eq!(
        projected.as_ref(),
        result_line.as_bytes(),
        "the answer stays the terminal result"
    );
    assert!(
        reduced_tool_names("claude", &raw).is_empty(),
        "the reduced result stores no tool calls"
    );
    let trace = traced("claude", &raw);
    assert_eq!(
        trace.usage,
        TokenUsage {
            input: 10,
            output: 2,
            ..Default::default()
        },
        "assistant usage is not added to the result"
    );
    let actual = trace
        .tool_calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                1,
                "Bash",
                serde_json::to_vec(&json!("listed")).unwrap().len() as u64
            ),
            (
                2,
                "Edit",
                serde_json::to_vec(&json!("patched")).unwrap().len() as u64
            ),
        ]
    );
    assert!(
        trace
            .tool_calls
            .iter()
            .all(|call| call.result_payload.is_none())
    );
    assert!(!format!("{:?}", trace.tool_calls).contains(patch));
}

// [ORB-20534] Muse's answer reduction keeps only the terminal text. Live edits
// are tool.result records; side_effect_intent is not a tool call, and the
// result text is counted rather than stored.
#[test]
fn muse_tool_result_records_edit_file_without_keeping_result_text() {
    let answer = json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null});
    let secret = "MUSE_RESULT_SECRET";
    let raw = [
        json!({"payload_type": "tool.result", "payload": {
            "call_id": "bash-1",
            "correlation_facts": {"tool_name": "bash"},
            "text": "listed",
        }}),
        json!({"payload_type": "tool.result", "payload": {
            "call_id": "edit-1",
            "correlation_facts": {"tool_name": "edit_file"},
            "text": "short",
        }}),
        json!({"payload_type": "tool.result", "payload": {
            "call_id": "edit-1",
            "correlation_facts": {"tool_name": "edit_file"},
            "text": secret,
        }}),
        json!({"payload_type": "task.lifecycle.side_effect_intent", "payload": {
            "call_id": "intent-1",
            "correlation_facts": {"tool_name": "write_file"},
            "text": secret,
        }}),
        json!({"payload_type": "run.terminal.completed", "payload": {
            "terminal": "completed",
            "text": answer.to_string(),
        }}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");

    let projected = project_cli_response("muse", raw.as_bytes());
    assert_eq!(
        projected.as_ref(),
        answer.to_string().as_bytes(),
        "the answer stays the completed terminal text"
    );
    assert!(
        reduced_tool_names("muse", &raw).is_empty(),
        "the reduced terminal text stores no tool calls"
    );
    let trace = traced("muse", &raw);
    let actual = trace
        .tool_calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (1, "bash", "listed".len() as u64),
            (2, "edit_file", secret.len() as u64)
        ]
    );
    assert!(
        trace
            .tool_calls
            .iter()
            .all(|call| call.result_payload.is_none())
    );
    assert!(!format!("{:?}", trace.tool_calls).contains(secret));
}

// [ORB-20534] `grok --output-format json` has no tool calls. streaming-json
// records them, while spend stays the final end event and the answer is the
// concatenated text chunks.
#[test]
fn grok_streaming_json_records_hashline_edit_from_the_end_event() {
    let envelope = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;
    let split = envelope.len() / 2;
    let (left, right) = envelope.split_at(split);
    let thought = "GROK_THOUGHT_SECRET";
    let patch = "GROK_PATCH_SECRET";
    let output = json!({"applied": true});
    let raw = [
        json!({"type": "text", "data": left}),
        json!({"type": "thought", "data": thought}),
        json!({"type": "usage", "usage": {"input_tokens": 9999, "output_tokens": 8888}, "total_cost_usd": 99.0}),
        json!({"type": "tool_call", "toolCallId": "call-1", "toolName": "hashline_edit", "rawInput": {"secret": patch}}),
        json!({"type": "tool_call_update", "toolCallId": "call-1", "rawOutput": output}),
        json!({"type": "tool_call", "toolCallId": "call-1", "toolName": "hashline_edit"}),
        json!({"type": "text", "data": right}),
        json!({"type": "tool_call", "toolCallId": "call-2", "toolName": "search_replace"}),
        json!({"type": "tool_call_update", "toolCallId": "unseen", "rawOutput": {"dropped": true}}),
        json!({"type": "end", "stopReason": "end_turn", "usage": {"input_tokens": 10, "output_tokens": 2}, "total_cost_usd": 0.5, "num_turns": 2}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");

    let projected = project_cli_response("grok", raw.as_bytes());
    assert_eq!(
        projected.as_ref(),
        envelope.as_bytes(),
        "text chunks concatenate with no separator"
    );
    assert!(
        !projected
            .as_ref()
            .windows(thought.len())
            .any(|window| window == thought.as_bytes())
    );
    let normalized = normalize_cli_stdout("grok", raw.as_bytes());
    let normalized = String::from_utf8(normalized.into_owned()).unwrap();
    assert!(
        !normalized.contains("9999"),
        "a per-response usage line is not added to the end event"
    );
    assert!(!normalized.contains(thought));
    assert!(
        !normalized.contains("hashline_edit"),
        "the usage document does not carry tool frames"
    );
    let trace = traced("grok", &raw);
    assert_eq!(
        trace.usage,
        TokenUsage {
            input: 10,
            output: 2,
            ..Default::default()
        }
    );
    assert_eq!(trace.provider_cost_usd, Some(0.5));
    let actual = trace
        .tool_calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                1,
                "hashline_edit",
                serde_json::to_vec(&output).unwrap().len() as u64
            ),
            (2, "search_replace", 0),
        ]
    );
    assert!(
        trace
            .tool_calls
            .iter()
            .all(|call| call.result_payload.is_none())
    );
    assert!(!format!("{:?}", trace.tool_calls).contains(patch));
}

#[test]
fn grok_multiline_wrapper_preserves_answer_and_document_telemetry() {
    let answer =
        r#"{"schemaVersion":1,"status":"success","result":{"source":"final-text"},"error":null}"#;
    let multiline = format!(
        r#"{{
"text":{},
"stopReason":"end_turn",
"metadata":[
{{"type":"tool_call","toolName":"read","toolCallId":"nested"}}
],
"usage":{{"input_tokens":10,"output_tokens":2}}
}}"#,
        json!(answer)
    );
    let wrapper: Value = serde_json::from_str(&multiline).unwrap();
    let compact = wrapper.to_string();

    for raw in [&compact, &multiline] {
        let response = project_cli_response("grok", raw.as_bytes());
        assert_eq!(response.as_ref(), answer.as_bytes());
        assert_eq!(
            normalize_cli_stdout("grok", raw.as_bytes()).as_ref(),
            raw.as_bytes()
        );
        assert_eq!(
            latest_assistant_message("grok", raw.as_bytes()).as_deref(),
            Some(answer)
        );
        assert!(
            recorded_tool_calls("grok", raw.as_bytes()).is_none(),
            "nested metadata is not a tool stream"
        );

        let response = String::from_utf8(response.into_owned()).unwrap();
        let parsed = ParsedStdout::parse(&response);
        parsed.response_envelope_protocol_check().unwrap();
        let (envelope, status, _) = parsed
            .parse_and_validate(&execution(response.clone()))
            .unwrap();
        assert_eq!(status, AgentResponseStatus::Success);
        assert_eq!(envelope.result.unwrap()["source"], "final-text");
        let trace = traced("grok", raw);
        assert_eq!(trace.usage.input, 10);
        assert_eq!(trace.usage.output, 2);
        assert!(trace.tool_calls.is_empty());
    }
}

// A malformed recognized Grok stream fails closed on every boundary:
// projection is empty, normalization never leaves the raw capture to be
// rescanned (only the `end` spend fields survive, with no answer), the
// newest-assistant-message reader sees nothing, and tool telemetry is
// still recorded from the parsed events without payloads.
#[test]
fn grok_malformed_stream_fails_closed_across_projection_and_telemetry() {
    let envelope =
        r#"{"schemaVersion":1,"status":"success","result":{"claimed":"tool-output"},"error":null}"#;
    let raw = [
        json!({"type": "tool_call", "toolCallId": "call-1", "toolName": "read", "rawInput": {"secret": "GROK_PATCH_SECRET"}}).to_string(),
        json!({"type": "tool_call_update", "toolCallId": "call-1", "rawOutput": envelope}).to_string(),
        json!({"type": "thought", "data": envelope}).to_string(),
        json!({"type": "end", "stopReason": "end_turn", "usage": {"input_tokens": 10, "output_tokens": 2}, "total_cost_usd": 0.5, "num_turns": 2}).to_string(),
        "warning".to_string(),
    ]
    .join("\n");

    assert!(project_cli_response("grok", raw.as_bytes()).is_empty());
    assert_eq!(latest_assistant_message("grok", raw.as_bytes()), None);

    let normalized = normalize_cli_stdout("grok", raw.as_bytes());
    let normalized_text = String::from_utf8(normalized.into_owned()).unwrap();
    assert_ne!(
        normalized_text, raw,
        "raw malformed capture must not survive normalization"
    );
    assert!(
        !normalized_text.contains("tool-output"),
        "{normalized_text}"
    );
    assert!(!normalized_text.contains("warning"), "{normalized_text}");
    let wrapper: Value = serde_json::from_str(&normalized_text).unwrap();
    assert_eq!(wrapper["text"], "");
    assert_eq!(wrapper["usage"]["input_tokens"], 10);
    assert_eq!(wrapper["usage"]["output_tokens"], 2);

    let calls = recorded_tool_calls("grok", raw.as_bytes())
        .expect("malformed stream is still recognized for telemetry");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].tool_name, "read");
    assert_eq!(
        calls[0].result_bytes,
        serde_json::to_vec(&json!(envelope)).unwrap().len() as u64
    );
    assert!(calls.iter().all(|call| call.result_payload.is_none()));
    assert!(!format!("{:?}", calls).contains("GROK_PATCH_SECRET"));
}

// [ORB-21951] A real `grok` 1.0.70 `streaming-json` capture interleaves
// `available_commands` frames with text chunks and reports each tool call
// twice (`tool_call`, then `tool_call_update` with a null `rawOutput` before
// the completed one). The recorded calls survive through the completion
// projection, including the `search_replace` edit a first-edit score needs.
#[test]
fn grok_real_stream_shape_records_read_and_edit_calls() {
    let envelope = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;
    let raw = [
        json!({"type": "available_commands", "tools": ["read_file", "search_replace"], "commands": []}).to_string(),
        json!({"type": "text", "data": envelope}).to_string(),
        json!({"type": "usage", "usage": {"input_tokens": 9999, "output_tokens": 8888}, "signature": "sig"}).to_string(),
        json!({"type": "tool_call", "toolCallId": "call-1", "title": "read_file", "kind": "read", "status": "pending", "toolName": "read_file", "rawInput": {"target_file": "probe.txt"}, "content": [], "locations": []}).to_string(),
        json!({"type": "tool_call_update", "toolCallId": "call-1", "status": null, "content": [], "rawOutput": null, "locations": [{"path": "probe.txt"}]}).to_string(),
        json!({"type": "tool_call_update", "toolCallId": "call-1", "status": "completed", "content": [], "rawOutput": {"type": "ReadFile", "FileContent": {"content": "hello"}}}).to_string(),
        json!({"type": "tool_call", "toolCallId": "call-2", "title": "search_replace", "kind": "edit", "status": "pending", "toolName": "search_replace", "rawInput": {"path": "probe.txt"}}).to_string(),
        json!({"type": "end", "stopReason": "end_turn", "usage": {"input_tokens": 10, "output_tokens": 2}, "total_cost_usd": 0.5, "num_turns": 2}).to_string(),
    ]
    .join("\n");

    let trace = traced("grok", &raw);
    let actual = trace
        .tool_calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(actual, vec![(1, "read_file"), (2, "search_replace")]);
    assert!(trace.tool_calls[0].result_bytes > 0);
    assert!(
        trace
            .tool_calls
            .iter()
            .all(|call| call.result_payload.is_none())
    );
}

// [ORB-21951] A real `agy` 1.4.12 `stream-json` capture reports tools only
// as `step_update` frames (`step_type: "tool"`); the terminal `result`
// document normalization keeps none of them. The raw stream records
// them through the same projection, and a stream with no tool step records
// an authoritative zero.
#[test]
fn antigravity_step_update_tools_are_recorded_through_projection() {
    let envelope = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;
    let raw = [
        json!({"event": "init", "conversation_id": "c1", "init": {"cwd": "/tmp", "tools": ["read_file", "write_to_file"]}}).to_string(),
        json!({"event": "step_update", "step_update": {"conversation_id": "c1", "step_index": 1, "state": "DONE", "step_type": "agent_response", "usage": {"input_tokens": 5, "output_tokens": 1}}}).to_string(),
        json!({"event": "step_update", "step_update": {"conversation_id": "c1", "step_index": 2, "state": "ACTIVE", "step_type": "tool", "tool_name": "read_file", "tool_info": {"name": "read_file", "parameters": {"Path": "probe.txt"}}}}).to_string(),
        json!({"event": "step_update", "step_update": {"conversation_id": "c1", "step_index": 2, "state": "DONE", "step_type": "tool", "tool_name": "read_file", "tool_info": {"name": "read_file", "parameters": {"Path": "probe.txt"}, "output": "1 lines, 17 bytes"}}}).to_string(),
        json!({"event": "step_update", "step_update": {"conversation_id": "c1", "step_index": 3, "state": "DONE", "step_type": "tool", "tool_name": "write_to_file", "tool_info": {"name": "write_to_file", "output": "edited"}}}).to_string(),
        json!({"event": "result", "result": {"conversation_id": "c1", "status": "SUCCESS", "response": envelope, "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12}}}).to_string(),
    ]
    .join("\n");

    let trace = traced("antigravity", &raw);
    let actual = trace
        .tool_calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                1,
                "read_file",
                serde_json::to_vec(&json!("1 lines, 17 bytes"))
                    .unwrap()
                    .len() as u64
            ),
            (
                2,
                "write_to_file",
                serde_json::to_vec(&json!("edited")).unwrap().len() as u64
            ),
        ]
    );

    let no_tools = [
        json!({"event": "init", "conversation_id": "c1", "init": {"cwd": "/tmp", "tools": []}}).to_string(),
        json!({"event": "result", "result": {"conversation_id": "c1", "status": "SUCCESS", "response": envelope, "usage": {"input_tokens": 1, "output_tokens": 1}}}).to_string(),
    ]
    .join("\n");
    assert!(
        traced("agy", &no_tools).tool_calls.is_empty(),
        "a recognized stream with no tool step records 0, with nothing fabricated"
    );
}

// [ORB-21951] Legacy Gemini CLI `stream-json` tool frames record through the
// same dispatch; its `-o json` document has no per-call events and is left
// to the generic parser (no fabricated calls).
#[test]
fn gemini_stream_tool_frames_are_recorded_and_json_document_is_left_alone() {
    let raw = [
        json!({"type": "init", "session_id": "s1", "model": "gemini-3.8-flash"}).to_string(),
        json!({"type": "tool_use", "tool_name": "read_file", "tool_id": "call-1", "parameters": {"path": "probe.txt"}}).to_string(),
        json!({"type": "tool_result", "tool_id": "call-1", "status": "success", "output": "hello"}).to_string(),
        json!({"type": "tool_use", "tool_name": "write_file", "tool_id": "call-2", "parameters": {}}).to_string(),
        json!({"type": "result", "status": "success", "stats": {}}).to_string(),
    ]
    .join("\n");
    let calls = recorded_tool_calls("gemini", raw.as_bytes()).expect("gemini stream is recognized");
    let actual = calls
        .iter()
        .map(|call| (call.seq, call.tool_name.as_str(), call.result_bytes))
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (
                1,
                "read_file",
                serde_json::to_vec(&json!("hello")).unwrap().len() as u64
            ),
            (2, "write_file", 0),
        ]
    );

    let document = json!({"response": "done", "stats": {"tools": {"totalCalls": 0}}}).to_string();
    assert!(
        recorded_tool_calls("gemini", document.as_bytes()).is_none(),
        "a single -o json document has no per-call events to record"
    );
    assert!(
        recorded_tool_calls("antigravity", b"not json").is_none(),
        "unrecognized output is left to the generic parser"
    );
}

/// The serialized provider schema stays inside the Claude structured-output
/// subset (no top-level conditional), the Claude CLI argv carries that same
/// schema, and the parser enforces the status/error correlation ([ORB-21567]).
#[test]
fn provider_schema_stays_in_claude_subset_and_parser_correlates_status() {
    let schema = response_envelope_json_schema();
    let arg = response_envelope_json_schema_arg();
    assert_eq!(
        serde_json::from_str::<Value>(&arg).unwrap(),
        schema,
        "the CLI argument must be the serialized response schema"
    );

    // Claude structured-output subset: no top-level conditional and none of
    // the constraint keywords that subset rejects with a 400 error.
    assert!(
        schema.get("anyOf").is_none(),
        "anyOf must stay nested: {schema}"
    );
    assert!(
        schema.get("oneOf").is_none(),
        "oneOf must stay nested: {schema}"
    );
    assert!(
        schema.get("allOf").is_none(),
        "allOf must stay nested: {schema}"
    );
    fn assert_supported_keywords(value: &Value) {
        match value {
            Value::Object(map) => {
                for (key, nested) in map {
                    assert!(
                        !matches!(
                            key.as_str(),
                            "minLength" | "maxLength" | "minimum" | "maximum" | "multipleOf"
                        ),
                        "Claude structured outputs reject {key}: {value}"
                    );
                    assert_supported_keywords(nested);
                }
            }
            Value::Array(items) => items.iter().for_each(assert_supported_keywords),
            _ => {}
        }
    }
    assert_supported_keywords(&schema);

    // The Claude invocation boundary receives this schema as --json-schema.
    let cfg = AgentConfig::cli("claude").expect("claude provider config");
    let (spec, _) = Agent::new(&cfg)
        .expect("claude provider adapter")
        .invoke(AgentRequest::activity(
            "fixture",
            br#"{"prompt":"fixture"}"#.to_vec(),
        ))
        .expect("claude invocation spec");
    let cli_schema = spec
        .args
        .windows(2)
        .find(|pair| pair[0] == "--json-schema")
        .map(|pair| pair[1].clone())
        .expect("claude invocation must carry --json-schema");
    assert_eq!(cli_schema, arg);

    let validator = jsonschema::JSONSchema::compile(&schema).expect("schema compiles");
    let valid = [
        json!({"schemaVersion": 1, "status": "success", "result": {}}),
        json!({"schemaVersion": 1, "status": "success", "result": {}, "durationMs": 12}),
        json!({"schemaVersion": 1, "status": "failed", "result": {},
               "error": {"code": "FAILURE", "message": "work failed"}}),
        json!({"schemaVersion": 1, "status": "failed", "result": {},
               "error": {"code": "FAILURE", "message": "work failed", "details": null}}),
        json!({"schemaVersion": 1, "status": "timeout", "result": {},
               "error": {"code": "AGENT_TIMEOUT", "message": "agent timed out"}}),
    ];
    for instance in &valid {
        assert!(
            validator.is_valid(instance),
            "schema must accept {instance}"
        );
    }
    let invalid = [
        json!({"schemaVersion": 1, "status": "failed", "result": {}, "error": "{}"}),
        json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null}),
        json!({"schemaVersion": 1, "status": "failed", "result": {},
               "error": {"code": "", "message": "work failed"}}),
        json!({"schemaVersion": 1, "status": "failed", "result": {},
               "error": {"code": "  ", "message": "work failed"}}),
        json!({"schemaVersion": 1, "status": "failed", "result": {},
               "error": {"code": "FAILURE", "message": ""}}),
        json!({"schemaVersion": 1, "status": "failed", "result": {},
               "error": {"code": "FAILURE"}}),
        json!({"schemaVersion": 1, "status": "timeout", "result": {},
               "error": {"code": "", "message": "agent timed out"}}),
    ];
    for instance in &invalid {
        assert!(
            !validator.is_valid(instance),
            "schema must reject {instance}"
        );
    }

    // The parser owns the status/error correlation.
    for (stdout, exit_code) in [
        (
            json!({"schemaVersion": 1, "status": "success", "result": {},
                   "error": {"code": "FAILURE", "message": "work failed"}})
            .to_string(),
            Some(0),
        ),
        (
            json!({"schemaVersion": 1, "status": "failed", "result": {}}).to_string(),
            Some(1),
        ),
        (
            json!({"schemaVersion": 1, "status": "failed", "result": {}, "error": null})
                .to_string(),
            Some(1),
        ),
        (
            json!({"schemaVersion": 1, "status": "timeout", "result": {}}).to_string(),
            Some(1),
        ),
        (
            json!({"schemaVersion": 1, "status": "timeout", "result": {}, "error": null})
                .to_string(),
            Some(1),
        ),
    ] {
        let mut exec = execution(stdout.clone());
        exec.exit_code = exit_code;
        exec.success = exit_code == Some(0);
        assert!(
            ParsedStdout::parse(&stdout)
                .parse_and_validate(&exec)
                .is_err(),
            "parser must reject schema-violating envelope: {stdout}"
        );
    }

    // A schema-less transport may still send `error: null` on success; the
    // parser treats JSON null as "no error" (ORB-21612).
    let stdout =
        json!({"schemaVersion": 1, "status": "success", "result": {}, "error": null}).to_string();
    assert!(
        ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .is_ok(),
        "parser must accept a null error on success: {stdout}"
    );
}

fn execution(stdout: String) -> ExecutionResult {
    ExecutionResult {
        success: true,
        timed_out: false,
        stdout,
        stderr: String::new(),
        exit_code: Some(0),
        duration_ms: 10,
        output: None,
    }
}
