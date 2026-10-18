//! Operator status writes cannot interrupt a local run's task ownership.

use std::path::Path;

use chrono::Utc;
use orbit_core::{ActorIdentity, OrbitRuntime, TaskStatus};
use orbit_engine::{RuntimeHost, TaskActivityUpdate};
use orbit_types::workflow::JobRunState;
use serde_json::json;

use crate::isolated_cli_fixture::Fixture;

#[test]
fn status_changes_respect_local_run_ownership_and_explicit_force() {
    if crate::isolated_test::run_in_child(
        "live_run_status::status_changes_respect_local_run_ownership_and_explicit_force",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let shown = fixture.json(&["workspace", "show", "--format", "json"]);
    assert_eq!(shown["registered"], true);
    assert_eq!(
        Path::new(shown["checkout"]["repo_root"].as_str().unwrap()),
        fixture.repo.canonicalize().unwrap()
    );
    assert_eq!(
        Path::new(shown["checkout"]["orbit_dir"].as_str().unwrap()),
        Path::new(shown["orbit_root"].as_str().unwrap())
            .canonicalize()
            .unwrap()
    );
    assert!(
        Path::new(shown["orbit_root"].as_str().unwrap())
            .starts_with(fixture._temp.path().canonicalize().unwrap())
    );
    let runtime = OrbitRuntime::from_roots(
        &fixture.root,
        Path::new(shown["orbit_root"].as_str().unwrap()),
    )
    .unwrap()
    .with_actor(ActorIdentity::human("live-run-fixture"));
    let id = fixture.json(&["task", "list", "--json"])[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The tool's proposal-approval route is a status write too, and a
    // submitted run owns its selection before it starts executing.
    let pending = RuntimeHost::insert_job_run(
        &runtime,
        "local-run-fixture",
        1,
        Utc::now(),
        Some(json!({"task_ids": [id]})),
        None,
    )
    .unwrap();
    let before = fixture.json(&["task", "show", &id, "--json"]);
    assert_approve_refused(&fixture, &id, &pending.run_id, &before);
    let input = json!({"id": id, "status": "backlog"}).to_string();
    let refused = fixture
        .command(&["tool", "run", "orbit.task.update", "--input", &input])
        .output()
        .unwrap();
    assert!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr).contains(&pending.run_id),
        "the tool approval path must preserve pending run ownership: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert_eq!(fixture.json(&["task", "show", &id, "--json"]), before);
    RuntimeHost::finalize_job_run(
        &runtime,
        &pending.run_id,
        JobRunState::Cancelled,
        Utc::now(),
        None,
    )
    .unwrap();
    // An unowned task still follows ordinary lifecycle transitions.
    fixture.json(&["task", "update", &id, "--status", "backlog", "--json"]);
    fixture.json(&["task", "update", &id, "--status", "in-progress", "--json"]);
    let run = RuntimeHost::insert_job_run(
        &runtime,
        "local-run-fixture",
        1,
        Utc::now(),
        Some(json!({"task_ids": [id]})),
        None,
    )
    .unwrap();
    RuntimeHost::mark_job_run_running(&runtime, &run.run_id, Utc::now(), std::process::id())
        .unwrap();
    // A newer run naming only a singular task_id must neither confer
    // ownership nor hide the older run whose task_ids names the task.
    RuntimeHost::insert_job_run(
        &runtime,
        "local-run-fixture",
        1,
        Utc::now(),
        Some(json!({"task_id": id})),
        None,
    )
    .unwrap();
    // The task deliberately has no job_run_id and no distributed claim.
    assert!(runtime.get_task(&id).unwrap().job_run_id.is_none());
    let before = fixture.json(&["task", "show", &id, "--json"]);
    let input = json!({"id": id, "status": "someday", "comment": "must not persist"}).to_string();
    for args in [
        vec!["task", "update", &id, "--status", "someday"],
        vec!["tool", "run", "orbit.task.update", "--input", &input],
    ] {
        let refused = fixture
            .command(&args)
            .env("ORBIT_OPERATOR", "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            !refused.status.success() && stderr.contains(&run.run_id),
            "a live local run must refuse operator CLI/tool status changes and name its run: {stderr}"
        );
        assert_eq!(fixture.json(&["task", "show", &id, "--json"]), before);
    }
    // Same-status and metadata writes remain usable during execution.
    let updated = fixture.json(&[
        "task",
        "update",
        &id,
        "--status",
        "in-progress",
        "--comment",
        "progress",
        "--json",
    ]);
    assert_eq!(updated["status"], "in-progress");
    assert_eq!(
        updated["comments"].as_array().unwrap().last().unwrap()["message"],
        "progress"
    );

    runtime
        .update_task_from_activity(
            &id,
            TaskActivityUpdate {
                status: TaskStatus::Review,
                expected_status: TaskStatus::InProgress,
                execution_summary: None,
                comment: None,
                note: None,
                agent: None,
                model: None,
                calling_run_id: Some(run.run_id.clone()),
            },
        )
        .unwrap();
    assert_eq!(
        fixture.json(&["task", "show", &id, "--json"])["status"],
        "review"
    );
    // The task still has no job_run_id and no execution claim. Approval must
    // refuse on the non-terminal run's task_ids alone, without writing a
    // comment or a history entry.
    assert!(runtime.get_task(&id).unwrap().job_run_id.is_none());
    assert!(
        runtime
            .inspect_execution_claims()
            .unwrap()
            .iter()
            .all(|inspection| inspection.claim.task_id != id)
    );
    let before_review = fixture.json(&["task", "show", &id, "--json"]);
    assert_approve_refused(&fixture, &id, &run.run_id, &before_review);
    let forced = fixture.json(&[
        "task", "update", &id, "--status", "someday", "--force", "--json",
    ]);
    assert_eq!(forced["status"], "someday");
    assert_eq!(
        forced["history"].as_array().unwrap().last().unwrap()["event"],
        "forced"
    );
    // MCP's special start path must not bypass the same guard.
    let input = json!({"id": id, "status": "in-progress"}).to_string();
    let refused = fixture
        .command(&["tool", "run", "orbit.task.update", "--input", &input])
        .output()
        .unwrap();
    assert!(
        !refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains(&run.run_id),
        "the tool start path must preserve local run ownership: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    RuntimeHost::finalize_job_run(
        &runtime,
        &run.run_id,
        JobRunState::Success,
        Utc::now(),
        None,
    )
    .unwrap();
    // Retained terminal history must not block the next operator write.
    assert_eq!(
        fixture.json(&["task", "update", &id, "--status", "backlog", "--json"])["status"],
        "backlog"
    );
    assert_eq!(
        fixture.json(&[
            "tool",
            "run",
            "orbit.task.update",
            "--input",
            &json!({"id": id, "status": "someday"}).to_string()
        ])["status"],
        "someday"
    );
    // Retained terminal history does not block the operator approval that
    // completes review.
    fixture.json(&[
        "task",
        "update",
        &id,
        "--status",
        "in-progress",
        "--plan",
        "Approve after the owner run ends.",
        "--json",
    ]);
    runtime
        .update_task_from_activity(
            &id,
            TaskActivityUpdate {
                status: TaskStatus::Review,
                expected_status: TaskStatus::InProgress,
                execution_summary: Some("Ready for approval.".to_string()),
                comment: None,
                note: None,
                agent: None,
                model: None,
                calling_run_id: None,
            },
        )
        .unwrap();
    assert_eq!(
        fixture.json(&["task", "update", &id, "--approve", "--json"])["status"],
        "done"
    );

    // Drain promotion calls approve_task while its own run still lists the
    // task. That trusted path stays available.
    let trusted = fixture.json(&[
        "task",
        "add",
        "--title",
        "Drain approval",
        "--complexity",
        "low",
        "--acceptance-criteria",
        "Trusted approval stays available",
        "--json",
    ]);
    let trusted_id = trusted["id"].as_str().unwrap();
    assert!(runtime.get_task(trusted_id).unwrap().job_run_id.is_none());
    assert!(
        runtime
            .inspect_execution_claims()
            .unwrap()
            .iter()
            .all(|inspection| inspection.claim.task_id != trusted_id)
    );
    let trusted_run = RuntimeHost::insert_job_run(
        &runtime,
        "local-run-fixture",
        1,
        Utc::now(),
        Some(json!({"task_ids": [trusted_id]})),
        None,
    )
    .unwrap();
    assert_eq!(
        runtime
            .approve_task(trusted_id, Some("drain promotion".to_string()), None)
            .unwrap()
            .status,
        TaskStatus::Backlog
    );
    assert_eq!(
        RuntimeHost::get_job_run(&runtime, &trusted_run.run_id)
            .unwrap()
            .unwrap()
            .state,
        JobRunState::Pending
    );
}

fn assert_approve_refused(fixture: &Fixture, id: &str, run_id: &str, before: &serde_json::Value) {
    let refused = fixture
        .command(&[
            "task",
            "update",
            id,
            "--approve",
            "--comment",
            "must not persist",
            "--note",
            "must not persist",
        ])
        .env("ORBIT_OPERATOR", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success() && stderr.contains(run_id),
        "CLI --approve must refuse while local run {run_id} owns the task and name that run: {stderr}"
    );
    assert_eq!(fixture.json(&["task", "show", id, "--json"]), *before);
}
