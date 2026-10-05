#![cfg(unix)]
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use orbit_core::{ActorIdentity, OrbitRuntime};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, blocked_workflow_failure_update};
use serde_json::Value;

use crate::isolated_cli_fixture;
use isolated_cli_fixture::Fixture;

#[test]
fn recheck_blocked_cli_previews_and_requeues_only_cleared_launchers_without_execution() {
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    let workspace_root = Path::new(shown["orbit_root"].as_str().unwrap());
    let runtime = OrbitRuntime::from_roots(&fixture.root, workspace_root)
        .unwrap()
        .with_actor(ActorIdentity::human("isolated-recheck-fixture"));
    let marker = fixture._temp.path().join("launcher-executed");
    let launcher = fixture._temp.path().join("fixture-launcher");
    let escaped_marker = marker.to_string_lossy().replace('\'', "'\"'\"'");
    fs::write(
        &launcher,
        format!("#!/bin/sh\nprintf executed > '{escaped_marker}'\nexit 1\n"),
    )
    .unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    let missing_launcher = fixture._temp.path().join("absent-launcher");
    let mut ids = Vec::new();
    for title in [
        "Cleared launcher",
        "Still missing launcher",
        "Task-level failure",
    ] {
        let task = fixture.json(&[
            "task",
            "add",
            "--title",
            title,
            "--complexity",
            "low",
            "--json",
        ]);
        ids.push(task["id"].as_str().unwrap().to_string());
    }
    let launcher_error = |program: &Path| {
        format!(
            "execution failed: v2 job dispatch: cli invocation failed (permanent): provider launcher `{}` for provider `codex` was not found; searched: /usr/bin/codex",
            program.display()
        )
    };
    for (id, run, error) in [
        (&ids[0], "jrun-recheck-cleared", launcher_error(&launcher)),
        (
            &ids[1],
            "jrun-recheck-missing",
            launcher_error(&missing_launcher),
        ),
        (
            &ids[2],
            "jrun-recheck-task",
            "step `implement_one` completed with success=false".to_string(),
        ),
    ] {
        runtime
            .apply_task_automation_update(
                id,
                TaskAutomationUpdate {
                    job_run_id: Some(run.into()),
                    ..blocked_workflow_failure_update(
                        "task_pr_pipeline",
                        run,
                        Some("STEP_FAILED"),
                        Some(&error),
                    )
                },
            )
            .unwrap();
    }
    let read = |id: &str| fixture.json(&["task", "show", id, "--json"]);
    let before: Vec<Value> = ids.iter().map(|id| read(id)).collect();
    let preview = fixture.json(&["task", "recheck-blocked", "--json"]);
    assert_eq!(preview["confirm"], false);
    assert_eq!(preview["requeued"], 0);
    let rows = preview["infra_blocked"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        2,
        "task-level failures are not infrastructure blocks"
    );
    assert_eq!(
        rows.iter().find(|r| r["task_id"] == ids[0]).unwrap()["launcher"],
        launcher.to_string_lossy().as_ref()
    );
    assert_eq!(
        rows.iter().find(|r| r["task_id"] == ids[1]).unwrap()["launcher"],
        Value::Null
    );
    assert_eq!(ids.iter().map(|id| read(id)).collect::<Vec<_>>(), before);
    let confirmed = fixture.json(&["task", "recheck-blocked", "--confirm", "--json"]);
    assert_eq!(confirmed["requeued"], 1);
    assert_eq!(read(&ids[0])["status"], "backlog");
    assert_eq!(
        read(&ids[1]),
        before[1],
        "missing launcher block is untouched"
    );
    assert_eq!(read(&ids[2]), before[2], "task failure is untouched");
    let history = runtime.get_task_history(&ids[0]).unwrap();
    let note = history.last().unwrap();
    assert_eq!(note.event, "infra_block_cleared");
    assert!(
        note.note
            .as_ref()
            .unwrap()
            .contains(launcher.to_str().unwrap())
    );
    assert!(
        note.note
            .as_ref()
            .unwrap()
            .contains("run_id=jrun-recheck-cleared")
    );
    let after = read(&ids[0]);
    assert_eq!(
        fixture.json(&["task", "recheck-blocked", "--confirm", "--json"])["requeued"],
        0
    );
    assert_eq!(read(&ids[0]), after);
    assert_eq!(
        runtime.get_task_history(&ids[0]).unwrap().len(),
        history.len()
    );
    assert!(
        !marker.exists(),
        "rechecking must resolve, never execute a provider launcher"
    );
}
