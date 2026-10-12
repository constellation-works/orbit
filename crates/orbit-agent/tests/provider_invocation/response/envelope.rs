//! Response discovery, failure synthesis and billing through ParsedStdout.

mod limits {
    use orbit_agent::ParsedStdout;
    use orbit_types::tool::ExecutionResult;
    use serde_json::{Value, json};

    fn exec(stdout: &str, exit_code: Option<i32>) -> ExecutionResult {
        ExecutionResult {
            success: exit_code == Some(0),
            timed_out: false,
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code,
            duration_ms: 12,
            output: None,
        }
    }

    fn assert_limit_error(error: &orbit_common::OrbitError) {
        let message = error.to_string();
        assert!(
            message.contains("response envelope discovery exceeded a work limit"),
            "expected an explicit discovery-limit failure, got {message}"
        );
    }

    fn dummy_object(width: usize) -> Value {
        let mut map = serde_json::Map::new();
        for index in 0..width {
            map.insert(format!("k{index}"), json!(index));
        }
        Value::Object(map)
    }

    #[test]
    fn exhausted_limits_do_not_synthesize_success() {
        let stdout = json!({
            "is_error": true,
            "subtype": "error_max_turns",
            "terminal_reason": "max_turns",
            "structured_output": dummy_object(70_000)
        })
        .to_string();
        let error = ParsedStdout::parse(&stdout)
            .parse_and_validate(&exec(&stdout, Some(0)))
            .expect_err("limit exhaustion is a parse failure");
        // The public entry point performs synthesis internally; returning this
        // discovery error proves it did not synthesize an outcome.
        assert_limit_error(&error);
    }
}

mod structured_output {
    use orbit_agent::{AgentResponseStatus, ParsedStdout};

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
                        let synthesized = ParsedStdout::parse(&stdout).parse_and_validate(&exec(
                            &stdout,
                            "",
                            exit_code,
                            exit_code == Some(0),
                        ));
                        if let Ok((envelope, status, _)) = synthesized {
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
                        assert!(
                            ParsedStdout::parse(&stdout)
                                .response_envelope_protocol_check()
                                .is_err()
                        );
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
        let (envelope, status, _) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&exec(&stdout, "", Some(0), true))
            .expect("live jrun-20260813-0451-3 wrapper with error string \"null\" is an envelope");

        assert_eq!(status, AgentResponseStatus::Success);
        assert_eq!(envelope.status, "success");
        assert!(envelope.error.is_none(), "string \"null\" is absent error");
        let result = envelope.result.expect("inner result object is kept");
        assert_eq!(result["task_id"], "ORB-10761");
        assert_eq!(result["summary"], "narrowed the checkoutless-client guard");
        ParsedStdout::parse(&stdout)
            .response_envelope_protocol_check()
            .expect("string-null error still satisfies the frame");
    }
}

mod usage {
    use serde_json::json;

    use orbit_types::telemetry::TokenUsage;

    use super::super::execution;
    use orbit_agent::ParsedStdout;
    #[test]
    fn claude_model_usage_is_not_added_on_top_of_sibling_usage() {
        // Live shape from jrun-20260720-0150-3 (Claude `--output-format json`).
        // `usage` and `modelUsage` describe the same billed session; summing
        // both used to persist 2x cache_read (ORB-10906). The usage rollup
        // also carries the cache-creation TTL split that modelUsage lacks.
        let mut wrapper = json!({
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
        });

        // Supply the completion frame required by the public response boundary;
        // the captured usage and modelUsage counters remain unchanged.
        wrapper["result"] = json!({"schemaVersion":1,"status":"success","result":{},"error":null})
            .to_string()
            .into();
        let stdout = wrapper.to_string();
        let (_, _, trace) = ParsedStdout::parse(&stdout)
            .parse_and_validate(&execution(stdout.clone()))
            .expect("usage capture parses");
        assert_eq!(
            trace.usage,
            TokenUsage {
                input: 212,
                cache_read: 11_470_771,
                cache_create: 51,
                cache_create_1h: 148_823,
                output: 46_418,
            }
        );
    }
}
