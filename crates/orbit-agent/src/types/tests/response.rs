#![allow(missing_docs)]

mod parse {
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
        let stdout = r#"{"type":"result","subtype":"success","result":"still waiting on the background run"}"#;
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
        let unsupported_version =
            r#"{"schemaVersion":2,"status":"success","result":{},"error":null}"#;
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
}

mod sum {
    #![allow(missing_docs)]

    use serde_json::json;

    use orbit_types::telemetry::TokenUsage;

    use super::super::super::response::usage::*;

    /// Gemini's `usageMetadata` reports reasoning beside the candidates;
    /// `totalTokenCount` is prompt + thoughts + candidates, so both count as
    /// output, and `promptTokenCount` stays the gross prompt total that the
    /// price table's gross basis later splits.
    #[test]
    fn gemini_usage_metadata_counts_thoughts_as_output() {
        let documents = vec![json!({
            "usageMetadata": {
                "promptTokenCount": 100_000,
                "cachedContentTokenCount": 90_000,
                "candidatesTokenCount": 800,
                "thoughtsTokenCount": 4_200,
                "totalTokenCount": 105_000
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                input: 100_000,
                cache_read: 90_000,
                cache_create: 0,
                cache_create_1h: 0,
                output: 5_000,
            }
        );
    }

    #[test]
    fn claude_cli_cache_creation_ttl_split_maps_each_ttl() {
        let documents = vec![json!({
            "usage": {
                "input_tokens": 36,
                "output_tokens": 8_265,
                "cache_read_input_tokens": 858_526,
                "cache_creation_input_tokens": 37_846,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 51,
                    "ephemeral_1h_input_tokens": 37_795,
                }
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                input: 36,
                cache_read: 858_526,
                cache_create: 51,
                cache_create_1h: 37_795,
                output: 8_265,
            }
        );
    }

    #[test]
    fn claude_model_usage_is_not_added_on_top_of_sibling_usage() {
        // Live shape from jrun-20260720-0150-3 (Claude `--output-format json`).
        // `usage` and `modelUsage` describe the same billed session; summing
        // both used to persist 2x cache_read (ORB-10906). The usage rollup
        // also carries the cache-creation TTL split that modelUsage lacks.
        let documents = vec![json!({
            "type": "result",
            "num_turns": 107,
            "usage": {
                "input_tokens": 212,
                "output_tokens": 46_418,
                "cache_read_input_tokens": 11_470_771,
                "cache_creation_input_tokens": 148_874,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 51,
                    "ephemeral_1h_input_tokens": 148_823,
                }
            },
            "modelUsage": {
                "claude-haiku-4-5-20251001": {
                    "inputTokens": 3856,
                    "outputTokens": 17,
                    "cacheReadInputTokens": 0,
                    "cacheCreationInputTokens": 0,
                    "costUSD": 0.003941,
                },
                "claude-sonnet-5": {
                    "inputTokens": 212,
                    "outputTokens": 46_418,
                    "cacheReadInputTokens": 11_470_771,
                    "cacheCreationInputTokens": 148_874,
                    "costUSD": 5.031381,
                }
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                input: 212,
                cache_read: 11_470_771,
                cache_create: 51,
                cache_create_1h: 148_823,
                output: 46_418,
            }
        );
    }

    #[test]
    fn claude_model_usage_is_collected_when_usage_rollup_is_absent() {
        let documents = vec![json!({
            "type": "result",
            "modelUsage": {
                "claude-haiku-4-5-20251001": {
                    "inputTokens": 100,
                    "outputTokens": 10,
                    "cacheReadInputTokens": 20,
                    "cacheCreationInputTokens": 5,
                },
                "claude-sonnet-5": {
                    "inputTokens": 200,
                    "outputTokens": 30,
                    "cacheReadInputTokens": 400,
                    "cacheCreationInputTokens": 15,
                }
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                input: 300,
                cache_read: 420,
                cache_create: 20,
                cache_create_1h: 0,
                output: 40,
            }
        );
    }

    #[test]
    fn cache_creation_without_a_ttl_split_remains_five_minute_usage() {
        let documents = vec![json!({
            "usage": {
                "cache_creation_input_tokens": 100,
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                cache_create: 100,
                ..TokenUsage::default()
            }
        );
    }

    #[test]
    fn gemini_cli_model_token_blocks_are_summed_once_per_model() {
        let documents = vec![json!({
            "stats": {
                "models": {
                    "gemini-3.1-pro": {
                        "tokens": {
                            "input": 10,
                            "cached": 2,
                            "candidates": 4,
                            "total": 999,
                            "thoughts": 70,
                            "tool": 30
                        },
                        "roles": {
                            "user": {
                                "tokens": {
                                    "input": 10,
                                    "cached": 2
                                }
                            },
                            "model": {
                                "tokens": {
                                    "candidates": 4
                                }
                            }
                        }
                    },
                    "gemini-2.5-flash": {
                        "tokens": {
                            "prompt": 20,
                            "cached": "3",
                            "output": "5",
                            "total": 28
                        },
                        "roles": {
                            "user": {
                                "tokens": {
                                    "prompt": 20
                                }
                            },
                            "model": {
                                "tokens": {
                                    "output": 5
                                }
                            }
                        }
                    }
                }
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                input: 30,
                cache_read: 5,
                cache_create: 0,
                cache_create_1h: 0,
                output: 109,
            }
        );
    }

    #[test]
    fn gemini_cli_role_tokens_are_counted_when_model_tokens_are_absent() {
        let documents = vec![json!({
            "stats": {
                "models": {
                    "gemini-3.1-pro": {
                        "roles": {
                            "user": {
                                "tokens": {
                                    "input": 7,
                                    "cached": 1
                                }
                            },
                            "model": {
                                "tokens": {
                                    "candidates": 3
                                }
                            }
                        }
                    }
                }
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                input: 7,
                cache_read: 1,
                cache_create: 0,
                cache_create_1h: 0,
                output: 3,
            }
        );
    }

    #[test]
    fn gemini_cli_thoughts_and_tool_are_folded_into_output() {
        let documents = vec![json!({
            "stats": {
                "models": {
                    "gemini-3.1-pro": {
                        "tokens": {
                            "total": 999,
                            "thoughts": 70,
                            "tool": 30
                        }
                    }
                }
            }
        })];

        // Gemini's `thoughts` and `tool` counters both consume the output
        // budget, so they sum into TokenUsage.output. `total` is a Gemini-side
        // rollup and is intentionally ignored to avoid double-counting.
        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                output: 100,
                ..TokenUsage::default()
            }
        );
    }

    #[test]
    fn gemini_cli_live_turn_shape_sums_thoughts_into_output() {
        let documents = vec![json!({
            "stats": {
                "models": {
                    "gemini-3.1-pro": {
                        "tokens": {
                            "input": 40919,
                            "output": 70,
                            "cached": 40101,
                            "thoughts": 396,
                            "tool": 0,
                            "total": 41385
                        }
                    }
                }
            }
        })];

        assert_eq!(
            sum_usage(&documents),
            TokenUsage {
                input: 40919,
                cache_read: 40101,
                cache_create: 0,
                cache_create_1h: 0,
                output: 466,
            }
        );
    }
}

/// [ORB-10746] The `--output-format json --json-schema` wrapper shape, and the
/// terminal endings structured output does not eliminate.
mod structured_output {
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
        response_envelope_protocol_check(&stdout)
            .expect("string-null error still satisfies the frame");
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
        let (envelope, status, trace) =
            parse_and_validate_response(&exec(&stdout, "", Some(0), true))
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
        let (envelope, status, trace) =
            parse_and_validate_response(&exec(&stdout, "", Some(0), true))
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
        let diagnostic =
            provider_invocation_diagnostic("", "error: unknown option '--json-schema'\n")
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
                        let synthesized = synthesize_response(&exec(
                            &stdout,
                            "",
                            exit_code,
                            exit_code == Some(0),
                        ));
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
}
