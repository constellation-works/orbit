use super::*;

#[test]
fn worktree_failure_keeps_json_out_of_the_message() {
    let host = StubHost {
        calls: Mutex::new(Vec::new()),
        respond: |_: &str, _: &Value| {
            Err(DispatchError::WorktreeIntegrity {
                code: "worktree_mismatch",
                diagnostic: json!({
                    "code": "worktree_mismatch",
                    "reason": "distinct checkouts require a declared, matching workspace_path/repo_root pair",
                    "requested_workspace_path": "/work/candidate",
                    "requested_repo_root": null,
                    "automatic_reconciliation": false,
                }).to_string(),
            })
        },
    };
    let job = job_asset(json!([{"id": "implement", "spec": {
        "type": "deterministic", "action": "missing_root", "config": {}
    }}]));
    let (result, events) = run_job_resume(&job, json!({}), "missing-root", &host, None);
    let message = result.unwrap_err().to_string();
    assert!(
        !message.contains('{')
            && !message.contains('\n')
            && message.contains("repo root")
            && message.contains("Declare"),
        "the missing-root failure must be one actionable line: {message}"
    );
    let event = events.iter().find(|event| matches!(
        &event.kind, V2AuditEventKind::StepFinished { step_id, .. } if step_id == "implement"
    )).expect("finished step");
    let event = serde_json::to_value(event).unwrap();
    assert_eq!(
        event["error_diagnostic"]["requested_workspace_path"],
        "/work/candidate"
    );
    assert_eq!(event["error_diagnostic"]["automatic_reconciliation"], false);
    assert!(!event["error_message"].as_str().unwrap().contains('{'));
}
