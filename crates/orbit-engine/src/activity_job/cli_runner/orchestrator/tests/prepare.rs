#![allow(missing_docs)]

use orbit_types::workflow::activity_job::V2AuditEventKind;

use super::super::super::super::dispatcher::DispatchError;
use super::super::super::tests::cli_run::CliRun;

#[test]
fn run_cli_backend_returns_error_when_declared_workspace_path_missing() {
    let cli = CliRun::new()
        .success_envelope()
        .run_id("job-missing-cwd")
        .audit_agent("codex:gpt-5.5");
    let missing = cli.root().join("missing-worktree");
    let mut out = cli
        .task_context(serde_json::json!({
            "workspace_path": missing.display().to_string()
        }))
        .input(serde_json::json!({
            "prompt": "do it",
            "task_id": "TMISSING"
        }))
        .run();

    let err = out
        .take_result()
        .expect_err("missing declared workspace should fail");
    match err {
        DispatchError::CliInvocationFailed(message) => {
            assert!(
                message.contains(&missing.display().to_string()),
                "error should name missing path: {message}"
            );
        }
        other => panic!("expected CliInvocationFailed, got {other:?}"),
    }

    let events = out.audit.events_snapshot().expect("events snapshot");
    assert!(
        !events
            .iter()
            .any(|event| matches!(&event.kind, V2AuditEventKind::CliInvocationStarted { .. })),
        "CliInvocationStarted should not be emitted before cwd validation succeeds"
    );
}

#[test]
fn run_cli_backend_records_resolved_cwd_in_started_event() {
    // The canonical workspace is the subject of the started event, so it stays
    // a second directory rather than the builder's agent root.
    let workspace_dir = tempfile::tempdir().expect("workspace tempdir");
    let workspace = workspace_dir
        .path()
        .canonicalize()
        .expect("canonical workspace");
    let workspace_string = workspace.display().to_string();

    let mut out = CliRun::new()
        .success_envelope()
        .run_id("job-cwd-audit")
        .audit_agent("codex:gpt-5.5")
        .task_context(serde_json::json!({
            "workspace_path": workspace_string.clone()
        }))
        .input(serde_json::json!({ "prompt": "do it", "task_id": "TCWD" }))
        .run();

    let outcome = out.take_result().expect("run succeeds");
    assert!(outcome.success);

    let events = out.audit.events_snapshot().expect("events snapshot");
    let cwd = events
        .iter()
        .find_map(|event| match &event.kind {
            V2AuditEventKind::CliInvocationStarted { cwd, .. } => cwd.as_deref(),
            _ => None,
        })
        .expect("cli.invocation.started cwd");
    assert_eq!(cwd, workspace_string);
}
