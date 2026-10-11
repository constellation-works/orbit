use orbit_agent::{AgentResponseStatus, ParsedStdout, normalize_cli_stdout, project_cli_response};
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
        String::from_utf8(project_cli_response("grok", grok.as_bytes()).into_owned()).unwrap(),
    ]
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
