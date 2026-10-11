use orbit_agent::{AgentResponseStatus, ParsedStdout, project_cli_response};
use orbit_types::tool::ExecutionResult;
use serde_json::json;

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
