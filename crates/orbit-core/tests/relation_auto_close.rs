#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

use chrono::{TimeZone, Utc};
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::friction_store::{FrictionAddParams, FrictionStore};
use orbit_types::record::FrictionStatus;
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use tempfile::TempDir;

fn test_runtime() -> (TempDir, OrbitRuntime, std::path::PathBuf) {
    let root = TempDir::new().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

/// The workspace-partitioned friction store this runtime reads and writes
/// (ORB-10680): records live in the host-global database keyed by
/// `(workspace_id, friction_id)`.
fn frictions(runtime: &OrbitRuntime) -> FrictionStore {
    FrictionStore::open(
        runtime.sqlite_store().expect("store"),
        runtime.workspace_id().expect("workspace id"),
        runtime.data_root().join("frictions"),
    )
    .expect("open friction store")
}

fn add_test_friction(runtime: &OrbitRuntime) -> String {
    let stored = frictions(runtime)
        .add(FrictionAddParams {
            model: "codex".to_string(),
            title: None,
            body: "Approval should close this friction".to_string(),
            tags: vec!["tooling".to_string()],
            during_task: None,
            created_at: Utc.with_ymd_and_hms(2026, 5, 17, 4, 5, 0).unwrap(),
        })
        .expect("add friction");
    stored.record.id
}

fn add_task_with_resolves(
    runtime: &OrbitRuntime,
    repo_root: &std::path::Path,
    target: &str,
    status: &str,
) -> String {
    let task = runtime
        .run_tool(
            "orbit.task.add",
            json!({
                "title": format!("Resolve {target}"),
                "description": "Fixture task with a resolves relation.",
                "acceptance_criteria": ["Relation is visible."],
                "complexity": "low",
                "workspace": repo_root.to_string_lossy(),
                "type": "feature",
                "relations": [
                    { "type": "resolves", "target": target }
                ],
                "model": "codex"
            }),
        )
        .expect("add task");
    let task_id = task["id"].as_str().expect("task id").to_string();
    // ORB-00255 retired `plan` and `status` from the orbit.task.add schema,
    // so seed the plan via update and approve into the desired status here.
    runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "plan": "1. Exercise the relation transition.",
                "model": "codex",
            }),
        )
        .expect("set plan via update");
    if status == "backlog" {
        runtime
            .run_tool(
                "orbit.task.update",
                json!({ "id": task_id, "status": "backlog", "model": "codex" }),
            )
            .expect("approve task into backlog");
    }
    task_id
}

fn move_backlog_task_to_review(runtime: &OrbitRuntime, task_id: &str) {
    runtime
        .run_tool(
            "orbit.task.update",
            json!({ "id": task_id, "status": "in_progress", "model": "codex" }),
        )
        .expect("start task");
    runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "status": "review",
                "execution_summary": "Ready for approval.",
                "model": "codex"
            }),
        )
        .expect("move task to review");
}

fn assert_friction_resolved_by(runtime: &OrbitRuntime, friction_id: &str, task_id: &str) {
    let friction = runtime
        .run_tool("orbit.friction.show", json!({ "id": friction_id }))
        .expect("show friction");
    assert_eq!(friction["status"], json!("resolved"));
    assert!(friction["resolved_at"].as_str().is_some());
    assert_eq!(friction["resolved_by_task"], json!(task_id));
}

#[test]
fn review_approval_resolves_related_friction_and_surfaces_json_fields() {
    let (_root, runtime, repo_root) = test_runtime();
    let friction_id = add_test_friction(&runtime);
    let task_id = add_task_with_resolves(&runtime, &repo_root, &friction_id, "backlog");
    move_backlog_task_to_review(&runtime, &task_id);

    runtime
        .run_tool(
            "orbit.task.update",
            json!({ "id": task_id, "status": "done", "model": "codex" }),
        )
        .expect("approve task");

    assert_friction_resolved_by(&runtime, &friction_id, &task_id);

    let task = runtime
        .run_tool("orbit.task.show", json!({ "id": task_id }))
        .expect("show task");
    assert_eq!(task["relations"][0]["type"], json!("resolves"));
    assert_eq!(task["relations"][0]["target"], json!(friction_id));
}

#[test]
fn task_update_to_done_resolves_related_friction() {
    let (_root, runtime, repo_root) = test_runtime();
    let friction_id = add_test_friction(&runtime);
    let task_id = add_task_with_resolves(&runtime, &repo_root, &friction_id, "backlog");
    move_backlog_task_to_review(&runtime, &task_id);

    let updated = runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "status": "done",
                "model": "codex"
            }),
        )
        .expect("update task to done");
    assert_eq!(updated["status"], json!("done"));

    assert_friction_resolved_by(&runtime, &friction_id, &task_id);
}

#[test]
fn automation_update_to_done_resolves_related_friction() {
    let (_root, runtime, repo_root) = test_runtime();
    let friction_id = add_test_friction(&runtime);
    let task_id = add_task_with_resolves(&runtime, &repo_root, &friction_id, "backlog");

    runtime
        .apply_task_automation_update(
            &task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect("automation update task to done");

    assert_friction_resolved_by(&runtime, &friction_id, &task_id);
}

#[test]
fn approving_task_with_dangling_friction_relation_records_event_but_succeeds() {
    let (_root, runtime, repo_root) = test_runtime();
    let task_id = add_task_with_resolves(&runtime, &repo_root, "F9999-12-999", "backlog");
    move_backlog_task_to_review(&runtime, &task_id);

    let approved = runtime
        .run_tool(
            "orbit.task.update",
            json!({ "id": task_id, "status": "done", "model": "codex" }),
        )
        .expect("approve task");
    assert_eq!(approved["status"], json!("done"));

    let events = runtime.list_session_events(20).expect("session events");
    assert!(events.iter().any(|event| {
        event.payload.get("type") == Some(&Value::String("TaskRelationDangling".to_string()))
            && event
                .payload
                .get("data")
                .and_then(Value::as_object)
                .is_some_and(|data| {
                    data.get("task_id") == Some(&Value::String(task_id.clone()))
                        && data.get("target") == Some(&Value::String("F9999-12-999".to_string()))
                })
    }));
    assert!(
        frictions(&runtime)
            .show("F9999-12-999")
            .expect("show dangling target")
            .is_none()
    );
}

#[test]
fn approving_task_does_not_overwrite_existing_friction_resolution() {
    let (_root, runtime, repo_root) = test_runtime();
    let friction_id = add_test_friction(&runtime);
    let original_resolved_at = Utc.with_ymd_and_hms(2026, 5, 17, 3, 0, 0).unwrap();
    frictions(&runtime)
        .resolve_by_task(&friction_id, "ORB-99999", original_resolved_at)
        .expect("pre-resolve friction");

    let task_id = add_task_with_resolves(&runtime, &repo_root, &friction_id, "backlog");
    move_backlog_task_to_review(&runtime, &task_id);
    runtime
        .run_tool(
            "orbit.task.update",
            json!({ "id": task_id, "status": "done", "model": "codex" }),
        )
        .expect("approve task");

    let stored = frictions(&runtime)
        .show(&friction_id)
        .expect("show friction")
        .expect("friction exists");
    assert_eq!(stored.record.status, FrictionStatus::Resolved);
    assert_eq!(stored.record.resolved_by_task.as_deref(), Some("ORB-99999"));
    assert_eq!(stored.record.resolved_at, Some(original_resolved_at));
}

#[test]
fn approving_proposed_task_does_not_resolve_friction() {
    let (_root, runtime, repo_root) = test_runtime();
    let friction_id = add_test_friction(&runtime);
    let task_id = add_task_with_resolves(&runtime, &repo_root, &friction_id, "proposed");

    let approved = runtime
        .run_tool(
            "orbit.task.update",
            json!({ "id": task_id, "status": "backlog", "model": "codex" }),
        )
        .expect("approve proposed task");
    assert_eq!(approved["status"], json!("backlog"));

    let stored = frictions(&runtime)
        .show(&friction_id)
        .expect("show friction")
        .expect("friction exists");
    assert_eq!(stored.record.status, FrictionStatus::Open);
    assert_eq!(stored.record.resolved_at, None);
    assert_eq!(stored.record.resolved_by_task, None);

    let task = runtime.get_task(&task_id).expect("get task");
    assert_eq!(task.status, TaskStatus::Backlog);
}

fn dual_workspace_runtimes() -> (
    TempDir,
    OrbitRuntime,
    std::path::PathBuf,
    OrbitRuntime,
    std::path::PathBuf,
) {
    let root = TempDir::new().expect("create tempdir");
    let global_root = root.path().join("global");
    std::fs::create_dir_all(&global_root).expect("create global root");

    let repo_a = root.path().join("repo_a");
    let orbit_a = repo_a.join(".orbit");
    std::fs::create_dir_all(&orbit_a).expect("create workspace a");
    let runtime_a =
        OrbitRuntime::from_roots(&global_root, &orbit_a).expect("build workspace a runtime");

    let repo_b = root.path().join("repo_b");
    let orbit_b = repo_b.join(".orbit");
    std::fs::create_dir_all(&orbit_b).expect("create workspace b");
    let runtime_b =
        OrbitRuntime::from_roots(&global_root, &orbit_b).expect("build workspace b runtime");

    (root, runtime_a, repo_a, runtime_b, repo_b)
}

fn assert_friction_not_local(error: &OrbitError, friction_id: &str, found_in: &str) {
    let details = error
        .friction_not_local_details()
        .expect("structured friction_not_local error");
    assert_eq!(details.friction_id, friction_id);
    assert!(
        details
            .found_in
            .iter()
            .any(|workspace| workspace == found_in),
        "expected owner {found_in}, got {:?}",
        details.found_in
    );
    assert_ne!(details.workspace_id, found_in);
}

fn assert_friction_still_open(runtime: &OrbitRuntime, friction_id: &str) {
    let stored = frictions(runtime)
        .show(friction_id)
        .expect("show friction")
        .expect("friction exists");
    assert_eq!(stored.record.status, FrictionStatus::Open);
    assert_eq!(stored.record.resolved_by_task, None);
}

/// F2026-08-094: a done task in one workspace named an unqualified friction
/// ID that lived in another, and auto-resolve silently no-op'd. Completing
/// that edge must now fail with `friction_not_local` and leave the foreign
/// friction open.
#[test]
fn cross_workspace_resolves_update_to_done_is_rejected() {
    let (_root, runtime_a, _repo_a, runtime_b, repo_b) = dual_workspace_runtimes();
    let friction_id = add_test_friction(&runtime_a);
    let owner = runtime_a.workspace_id().expect("workspace a id");
    let task_id = add_task_with_resolves(&runtime_b, &repo_b, &friction_id, "backlog");
    move_backlog_task_to_review(&runtime_b, &task_id);

    let error = runtime_b
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "status": "done",
                "model": "codex"
            }),
        )
        .expect_err("cross-workspace resolves must not complete");
    assert_friction_not_local(&error, &friction_id, &owner);
    assert_friction_still_open(&runtime_a, &friction_id);
    assert_eq!(
        runtime_b.get_task(&task_id).expect("get task").status,
        TaskStatus::Review
    );
}

#[test]
fn cross_workspace_resolves_review_approval_is_rejected() {
    let (_root, runtime_a, _repo_a, runtime_b, repo_b) = dual_workspace_runtimes();
    let friction_id = add_test_friction(&runtime_a);
    let owner = runtime_a.workspace_id().expect("workspace a id");
    let task_id = add_task_with_resolves(&runtime_b, &repo_b, &friction_id, "backlog");
    move_backlog_task_to_review(&runtime_b, &task_id);

    let error = runtime_b
        .run_tool(
            "orbit.task.update",
            json!({ "id": task_id, "status": "done", "model": "codex" }),
        )
        .expect_err("cross-workspace resolves must not approve into done");
    assert_friction_not_local(&error, &friction_id, &owner);
    assert_friction_still_open(&runtime_a, &friction_id);
    assert_eq!(
        runtime_b.get_task(&task_id).expect("get task").status,
        TaskStatus::Review
    );
}

#[test]
fn cross_workspace_resolves_automation_update_is_rejected() {
    let (_root, runtime_a, _repo_a, runtime_b, repo_b) = dual_workspace_runtimes();
    let friction_id = add_test_friction(&runtime_a);
    let owner = runtime_a.workspace_id().expect("workspace a id");
    let task_id = add_task_with_resolves(&runtime_b, &repo_b, &friction_id, "backlog");

    let error = runtime_b
        .apply_task_automation_update(
            &task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                ..TaskAutomationUpdate::default()
            },
        )
        .expect_err("cross-workspace resolves must not complete via automation");
    assert_friction_not_local(&error, &friction_id, &owner);
    assert_friction_still_open(&runtime_a, &friction_id);
    assert_eq!(
        runtime_b.get_task(&task_id).expect("get task").status,
        TaskStatus::Backlog
    );
}

#[test]
fn same_workspace_resolves_still_wins_when_another_workspace_shares_the_id() {
    let (_root, runtime_a, _repo_a, runtime_b, repo_b) = dual_workspace_runtimes();
    let foreign_id = add_test_friction(&runtime_a);
    let local_id = add_test_friction(&runtime_b);
    assert_eq!(foreign_id, local_id);

    let task_id = add_task_with_resolves(&runtime_b, &repo_b, &local_id, "backlog");
    move_backlog_task_to_review(&runtime_b, &task_id);
    runtime_b
        .run_tool(
            "orbit.task.update",
            json!({
                "id": task_id,
                "status": "done",
                "model": "codex"
            }),
        )
        .expect("local resolves still completes");

    assert_friction_resolved_by(&runtime_b, &local_id, &task_id);
    assert_friction_still_open(&runtime_a, &foreign_id);
}
