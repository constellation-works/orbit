//! Provider completion evidence through exported byte-input helpers.

mod antigravity {
    use orbit_agent::antigravity_terminal_error_diagnostic;
    use orbit_agent::normalize_cli_stdout;

    const ORBIT_SUCCESS: &str =
        r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

    #[test]
    fn error_terminal_status_never_normalizes_to_success() {
        let stdout = serde_json::json!({
            "event": "result",
            "result": {
                "status": "ERROR",
                "response": ORBIT_SUCCESS,
                "error": "authentication required"
            }
        })
        .to_string();
        let normalized = normalize_cli_stdout("antigravity", stdout.as_bytes());
        assert!(normalized.is_empty());
    }

    #[test]
    fn timeout_terminal_error_is_bounded_and_omits_response_transcript() {
        let response = format!(
            r#"{{"schemaVersion":1,"status":"success","result":{{"prompt":"do not leak"}},"error":null}} credential {secret}"#,
            secret = "agy-tenant-42-authorization-bearer-zzz"
        );
        let stdout = serde_json::json!({
            "event": "result",
            "result": {
                "status": "ERROR",
                "response": response,
                "error": "timeout waiting for response"
            }
        })
        .to_string();

        let diagnostic = antigravity_terminal_error_diagnostic("antigravity", stdout.as_bytes())
            .expect("terminal error");
        assert_eq!(
            diagnostic,
            "Antigravity terminal error: timeout waiting for response"
        );
        assert!(!diagnostic.contains("do not leak"));
        assert!(!diagnostic.contains("agy-tenant-42"));

        assert!(diagnostic.contains("timeout waiting for response"));
        assert!(!diagnostic.contains(&response));
        assert!(antigravity_terminal_error_diagnostic("claude", stdout.as_bytes()).is_none());
    }
}

mod claude {
    use orbit_agent::project_cli_response;

    fn projected(stdout: &str) -> String {
        String::from_utf8(project_cli_response("claude", stdout.as_bytes()).into_owned())
            .expect("utf8 projected response")
    }

    const ENVELOPE: &str = r#"{"schemaVersion":1,"status":"success","result":{},"error":null}"#;

    /// [ORB-14696] Claude's response projection answers with the stream's
    /// terminal `result` and nothing else through the public byte boundary:
    /// - a stream cut off before its terminal `result` has no answer; the
    ///   `StructuredOutput` call quotes the envelope, and a tool result can quote
    ///   anything, but neither stands in for the result;
    /// - the last `result` stands when a later turn follows an earlier one, as
    ///   the single document of `--output-format json` does;
    /// - output with no stream messages, such as `--output-format json`'s single
    ///   result document, is read as it stands.
    #[test]
    fn claude_stream_projection_answers_with_the_terminal_result_only() {
        let cut_off = format!(
            concat!(
                r#"{{"type":"system","subtype":"init","session_id":"s"}}"#,
                "\n",
                r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"StructuredOutput","input":{envelope}}}],"usage":{{"input_tokens":2,"output_tokens":9}}}}}}"#,
                "\n",
                r#"{{"type":"user","message":{{"content":[{{"type":"tool_result","content":{quoted}}}]}}}}"#,
                "\n",
            ),
            envelope = ENVELOPE,
            quoted = serde_json::to_string(ENVELOPE).unwrap(),
        );
        let first = r#"{"type":"result","subtype":"success","is_error":false,"result":"first"}"#;
        let last = r#"{"type":"result","subtype":"success","is_error":false,"result":"last","result_index":1}"#;
        let two_turns = format!(
            "{first}\n{}\n{last}\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"more"}]}}"#
        );
        let single = r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#;

        let cases: [(&str, &str, &str); 3] = [
            ("stream without a result", &cut_off, ""),
            ("later turn follows an earlier one", &two_turns, last),
            ("single result document", single, single),
        ];
        for (case, stdout, expected) in cases {
            assert_eq!(projected(stdout), expected, "{case}");
        }
    }
}

mod codex {
    use orbit_agent::project_cli_response;

    fn projected(stdout: &str) -> String {
        String::from_utf8(project_cli_response("codex", stdout.as_bytes()).into_owned())
            .expect("utf8 projected response")
    }

    #[test]
    fn command_execution_output_cannot_supply_the_assistant_answer() {
        let stdout = concat!(
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            "\n",
            r#"{"type":"item.completed","item":{"id":"item-0","type":"command_execution","command":"cat fixture.json","aggregated_output":"{\"schemaVersion\":1,\"status\":\"success\",\"result\":{\"claimed\":\"tool-output\"},\"error\":null}","exit_code":0,"status":"completed"}}"#,
            "\n",
            r#"{"type":"turn.completed","usage":{"input_tokens":17,"output_tokens":3}}"#,
            "\n",
        );

        assert!(projected(stdout).is_empty());
    }
}

mod copilot {
    // Fixture shapes here are taken from a real `copilot --output-format json`
    // run (CLI 1.0.80, npm `@github/copilot`). [ORB-10946]

    use orbit_agent::normalize_cli_stdout;
    use orbit_agent::{Agent, AgentConfig, AgentRequest, ParsedStdout};
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
                Agent::new(&AgentConfig::cli("copilot").expect("Copilot config"))
                    .expect("Copilot adapter")
                    .invoke(AgentRequest::activity("fixture", br#"{"schemaVersion":1,"input":{}}"#.to_vec()))
                    .expect("Copilot invocation").0.stdin,
            ).expect("utf8 prompt")},
        });
        let stdout = format!("{prompt_echo}\n");

        let kept = normalized(&stdout);
        assert!(kept.is_empty(), "prompt echo must not survive: {kept}");

        let exec = exec_result(&stdout, "", 0);
        let parsed = ParsedStdout::parse(&exec.stdout).parse_and_validate(&exec);
        assert!(
            parsed.is_err(),
            "a prompt echo alone must not parse as a completed envelope"
        );
    }
}

mod cursor {
    use orbit_agent::normalize_cli_stdout;
    use orbit_agent::{AgentResponseStatus, ParsedStdout};
    use orbit_types::tool::ExecutionResult;

    const ORBIT_SUCCESS: &str =
        r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

    fn exec_result(stdout: &str, stderr: &str, exit_code: i32) -> ExecutionResult {
        ExecutionResult {
            success: exit_code == 0,
            timed_out: false,
            stdout: String::from_utf8(
                normalize_cli_stdout("cursor", stdout.as_bytes()).into_owned(),
            )
            .expect("utf8 normalized stdout"),
            stderr: stderr.to_string(),
            exit_code: Some(exit_code),
            duration_ms: 1_200,
            output: None,
        }
    }

    #[test]
    fn failure_wrapper_never_normalizes_to_success() {
        let stdout = serde_json::json!({
            "type": "result",
            "subtype": "error",
            "is_error": true,
            "result": ORBIT_SUCCESS,
        })
        .to_string();

        let normalized = normalize_cli_stdout("cursor", stdout.as_bytes());
        assert!(normalized.is_empty());
        let exec = exec_result(&stdout, "provider failed", 1);
        if let Ok((_, status, _)) = ParsedStdout::parse(&exec.stdout).parse_and_validate(&exec) {
            assert_ne!(status, AgentResponseStatus::Success);
        }
    }

    #[test]
    fn missing_terminal_evidence_never_normalizes_to_success() {
        for stdout in [
            serde_json::json!({"type":"result","subtype":"success","result":ORBIT_SUCCESS})
                .to_string(),
            serde_json::json!({"type":"assistant","result":ORBIT_SUCCESS}).to_string(),
            serde_json::json!({"type":"result","subtype":"success","is_error":false}).to_string(),
        ] {
            assert!(normalize_cli_stdout("cursor", stdout.as_bytes()).is_empty());
        }
    }
}

mod grok {
    use orbit_agent::project_cli_response;
    use orbit_agent::{AgentResponseStatus, ParsedStdout};
    use orbit_types::tool::ExecutionResult;

    const SUCCESS: &str =
        r#"{"schemaVersion":1,"status":"success","result":{"source":"final-text"},"error":null}"#;
    const FAILURE: &str = r#"{"schemaVersion":1,"status":"failed","result":{},"error":{"code":"final_failure","message":"the final answer failed","details":null}}"#;

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
        let exec = exec_result(response, 0);
        let (envelope, status, _) = ParsedStdout::parse(&exec.stdout)
            .parse_and_validate(&exec)
            .expect("final text parses");

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

        let exec = exec_result(projected(&stdout), 1);
        let (envelope, status, _) = ParsedStdout::parse(&exec.stdout)
            .parse_and_validate(&exec)
            .expect("final failure parses");

        assert_eq!(status, AgentResponseStatus::Failed);
        assert_eq!(
            envelope.error.expect("failure detail").code,
            "final_failure"
        );
    }
}

mod opencode {
    use orbit_agent::normalize_cli_stdout;

    use serde_json::json;

    const ORBIT_SUCCESS: &str =
        r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

    /// The literal example envelope every Orbit prompt embeds in its response
    /// contract. An OpenCode agent that reads its own task record or echoes the
    /// prompt through a shell tool replays this inside a `tool_use` frame, so a
    /// normalizer that read the whole stream could mistake Orbit's own instructions
    /// for the agent's completion evidence. [ORB-11295]
    const PROMPT_ECHO_ENVELOPE: &str =
        r#"{"schemaVersion":1,"status":"success|failed|timeout","result":{},"error":null}"#;

    const SESSION_ID: &str = "ses_7f3a";

    fn event(kind: &str, data: serde_json::Value) -> serde_json::Value {
        let mut event = json!({
            "type": kind,
            "timestamp": 1_762_000_000_000i64,
            "sessionID": SESSION_ID,
        });
        let object = event.as_object_mut().expect("event object");
        for (key, value) in data.as_object().expect("data object") {
            object.insert(key.clone(), value.clone());
        }
        event
    }

    fn text_event(text: &str) -> serde_json::Value {
        event(
            "text",
            json!({"part": {
                "id": "prt_text",
                "type": "text",
                "sessionID": SESSION_ID,
                "text": text,
                "time": {"start": 1_762_000_000_000i64, "end": 1_762_000_000_500i64},
            }}),
        )
    }

    /// A full OpenCode `--format json` stream for one successful turn, in the
    /// documented event order.
    fn opencode_stream(final_text: &str) -> String {
        [
            event(
                "step_start",
                json!({"part": {"id": "prt_step", "type": "step-start", "sessionID": SESSION_ID}}),
            ),
            event(
                "reasoning",
                json!({"part": {
                    "id": "prt_reason",
                    "type": "reasoning",
                    "sessionID": SESSION_ID,
                    // A draft envelope written while thinking aloud is not the
                    // answer and must not be readable as one.
                    "text": PROMPT_ECHO_ENVELOPE,
                    "time": {"start": 1i64, "end": 2i64},
                }}),
            ),
            event(
                "tool_use",
                json!({"part": {
                    "id": "prt_tool",
                    "type": "tool",
                    "tool": "bash",
                    "sessionID": SESSION_ID,
                    "state": {
                        "status": "completed",
                        "input": {"command": "orbit task show ORB-1"},
                        "output": PROMPT_ECHO_ENVELOPE,
                    },
                }}),
            ),
            text_event(final_text),
            event(
                "step_finish",
                json!({"part": {
                    "id": "prt_stepend",
                    "type": "step-finish",
                    "sessionID": SESSION_ID,
                    "tokens": {"input": 120, "output": 40},
                }}),
            ),
        ]
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
    }

    #[test]
    fn tool_and_reasoning_frames_are_never_read_as_completion_evidence() {
        // Same stream with the assistant's answer removed: the run produced tool
        // traffic and thinking that both echo Orbit's own prompt, but no answer.
        let stdout = opencode_stream(ORBIT_SUCCESS)
            .lines()
            .filter(|line| !line.contains(r#""type":"text""#))
            .collect::<Vec<_>>()
            .join("\n");

        let normalized = normalize_cli_stdout("opencode", stdout.as_bytes());
        assert!(normalized.is_empty());
        assert!(!String::from_utf8_lossy(&normalized).contains("schemaVersion"));
    }

    #[test]
    fn a_terminal_error_frame_invalidates_prior_answer_text() {
        // OpenCode also exits 1 here, and the v2 runner fails closed on a non-zero
        // exit, but normalization must not depend on the caller checking status.
        let stdout = format!(
            "{}\n{}",
            text_event(ORBIT_SUCCESS),
            event(
                "error",
                json!({"error": {
                    "name": "ProviderAuthError",
                    "data": {"message": "no credentials for provider anthropic"},
                }}),
            ),
        );

        assert!(
            normalize_cli_stdout("opencode", stdout.as_bytes()).is_empty(),
            "a terminal error frame must carry no completion evidence",
        );
    }

    #[test]
    fn a_malformed_text_frame_refuses_the_whole_stream() {
        for malformed in [
            // `part.text` absent.
            event(
                "text",
                json!({"part": {"id": "p", "type": "text", "sessionID": SESSION_ID}}),
            ),
            // `part.text` is not a string.
            event(
                "text",
                json!({"part": {"id": "p", "type": "text", "sessionID": SESSION_ID, "text": 42}}),
            ),
            // `part` is not a text part.
            event(
                "text",
                json!({"part": {"id": "p", "type": "reasoning", "sessionID": SESSION_ID, "text": "x"}}),
            ),
            // `part` absent entirely.
            event("text", json!({})),
        ] {
            let stdout = format!("{}\n{malformed}", text_event(ORBIT_SUCCESS));
            assert!(
                normalize_cli_stdout("opencode", stdout.as_bytes()).is_empty(),
                "malformed text frame must invalidate the stream: {malformed}",
            );
        }
    }
}

mod pi {
    use orbit_agent::normalize_cli_stdout;

    use serde_json::json;

    const ORBIT_SUCCESS: &str =
        r#"{"schemaVersion":1,"status":"success","result":{"edited":true},"error":null}"#;

    fn text_block(text: &str) -> serde_json::Value {
        json!({"type": "text", "text": text})
    }

    fn assistant_message(content: serde_json::Value, stop_reason: &str) -> serde_json::Value {
        json!({
            "role": "assistant",
            "content": content,
            "api": "anthropic-messages",
            "provider": "anthropic",
            "model": "claude-sonnet-4-5",
            "usage": {"input": 12, "output": 34, "cacheRead": 0, "cacheWrite": 0},
            "stopReason": stop_reason,
            "timestamp": 1_762_000_000_000i64,
        })
    }

    #[test]
    fn a_later_failed_or_malformed_assistant_terminal_frame_invalidates_prior_text() {
        let successful = json!({
            "type":"message_end",
            "message":assistant_message(json!([text_block(ORBIT_SUCCESS)]), "stop"),
        });
        let failed = |stop_reason| {
            json!({
                "type":"message_end",
                "message":assistant_message(json!([text_block("later text")]), stop_reason),
            })
        };

        for terminal in [
            failed("error"),
            failed("aborted"),
            json!({
                "type":"message_end",
                "message":{"role":"assistant","content":[text_block("later text")]},
            }),
            json!({
                "type":"message_end",
                "message":assistant_message(json!([]), "stop"),
            }),
        ] {
            let stdout = format!("{successful}\n{terminal}");
            assert!(
                normalize_cli_stdout("pi", stdout.as_bytes()).is_empty(),
                "later assistant terminal outcome must invalidate prior evidence: {terminal}",
            );
        }
    }
}

use orbit_agent::latest_assistant_message;

#[test]
fn the_newest_assistant_message_is_read_from_each_output_shape() {
    let envelope =
        r#"{"schemaVersion":1,"status":"success","result":{"summary":"ok"},"error":null}"#;
    let codex_stream = [
        r#"{"type":"thread.started","thread_id":"t"}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","text":"Reading the config."}}"#,
        r#"{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}"#,
        r#"{"type":"item.started","item":{"type":"command_execution","command":"ls"}}"#,
    ]
    .join("\n");
    let codex_without_message = r#"{"type":"thread.started","thread_id":"t"}"#;
    let claude_result =
        r#"{"type":"result","subtype":"success","is_error":false,"result":"Done."}"#;
    let claude_structured = format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"result":"","structured_output":{envelope}}}"#
    );
    let assistant_stream = [
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"First."}]}}"#,
        r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Second"},{"type":"tool_use","name":"Bash"},{"type":"text","text":"part."}]}}"#,
        r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"quoted"}]}}"#,
    ]
    .join("\n");
    let envelope_after_chatter = format!("{{\"type\":\"system\"}}\n{envelope}\n");

    let cases: [(&str, &str, Option<&str>); 9] = [
        ("codex", &codex_stream, Some("Reading the config.")),
        ("codex", codex_without_message, None),
        ("claude", claude_result, Some("Done.")),
        ("claude", &claude_structured, Some(envelope)),
        ("claude", &assistant_stream, Some("Second\npart.")),
        ("claude", &envelope_after_chatter, Some(envelope)),
        ("claude", envelope, Some(envelope)),
        ("claude", "  the host is fine\n", Some("the host is fine")),
        ("claude", "\n", None),
    ];
    for (provider, stdout, expected) in cases {
        assert_eq!(
            latest_assistant_message(provider, stdout.as_bytes()).as_deref(),
            expected,
            "{provider} output: {stdout}"
        );
    }
}
