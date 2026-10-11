use orbit_agent::{AgentResponseStatus, ParsedStdout, project_cli_response};
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
