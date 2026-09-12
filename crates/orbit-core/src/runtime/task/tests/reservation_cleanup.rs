//! Sibling tests for `reservation_cleanup.rs`.

use crate::OrbitRuntime;
use chrono::Utc;
use orbit_store::contracts::TaskReservationReleaseReason;
use orbit_types::workflow::JobRunState;
use serde_json::json;

use super::super::locks::{workspace_orbit_dir, workspace_task_reservation_id};

use chrono::Duration;
use orbit_store::{TaskCreateParams, TaskReservationReserveParams};
use orbit_tools::{ReservationOwnerContext, ToolContext};
use orbit_types::policy::Role;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::Value;
use std::collections::BTreeSet;
use tempfile::tempdir;

fn operator_session() -> ToolSessionContext {
    ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        ..ToolSessionContext::default()
    }
}

fn test_runtime() -> (tempfile::TempDir, OrbitRuntime, std::path::PathBuf) {
    let root = tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build test runtime");
    (root, runtime, repo_root)
}

fn create_context_task(
    runtime: &OrbitRuntime,
    repo_root: &std::path::Path,
    id_hint: &str,
    context_file: &str,
) -> String {
    create_context_task_with_status(
        runtime,
        repo_root,
        id_hint,
        context_file,
        TaskStatus::Backlog,
    )
}

fn create_context_task_with_status(
    runtime: &OrbitRuntime,
    _repo_root: &std::path::Path,
    id_hint: &str,
    context_file: &str,
    status: TaskStatus,
) -> String {
    let task = runtime
        .stores()
        .task_records()
        .create(TaskCreateParams {
            actor: "test".to_string(),
            parent_id: None,
            title: format!("task {id_hint}"),
            description: "test".to_string(),
            acceptance_criteria: Vec::new(),
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: String::new(),
            execution_summary: String::new(),
            context_files: vec![context_file.to_string()],
            repo_root: None,
            created_by: Some("test".to_string()),
            planned_by: None,
            implemented_by: None,
            status,
            priority: TaskPriority::Medium,
            complexity: None,
            task_type: TaskType::Chore,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            orchestrator: None,
            comments: Vec::new(),
        })
        .expect("create task");
    task.id
}

fn reserve_direct(
    runtime: &OrbitRuntime,
    task_ids: Vec<String>,
    file: &str,
    owner_run_id: Option<&str>,
) -> String {
    runtime
        .stores()
        .task_reservations()
        .reserve_task_reservation(TaskReservationReserveParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime).expect("workspace reservation id"),
            task_ids,
            requested_files: vec![file.to_string()],
            actor: "test".to_string(),
            ttl_seconds: 3600,
            owner_run_id: owner_run_id.map(str::to_string),
            owner_metadata_json: None,
        })
        .expect("reserve directly")
        .reservation_id
        .expect("reservation id")
}

fn insert_running_run(
    runtime: &OrbitRuntime,
    job_id: &str,
    pid: u32,
) -> orbit_types::workflow::JobRun {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_id, 1, Utc::now() - Duration::seconds(5), None, None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now() - Duration::seconds(3), pid)
        .expect("mark running");
    runtime
        .stores()
        .jobs()
        .get_job_run(&run.run_id)
        .expect("load run")
        .expect("run exists")
}

fn reserve_via_tool_for_owner(runtime: &OrbitRuntime, owner_run_id: &str, task_id: &str) -> String {
    let output = runtime
        .run_tool_with_context_and_role(
            "orbit.task.locks.reserve",
            json!({
                "task_ids": [task_id],
                "ttl_seconds": 3600,
                "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
                "owner_run_id": "caller-controlled-value-is-ignored",
            }),
            Role::Admin,
            ToolContext {
                session_context: operator_session(),
                reservation_owner: Some(ReservationOwnerContext {
                    owner_run_id: owner_run_id.to_string(),
                    owner_metadata_json: Some(r#"{"source":"test"}"#.to_string()),
                }),
                ..ToolContext::default()
            },
        )
        .expect("reserve via tool");
    assert_eq!(output["reserved"], true);
    output["reservation_id"]
        .as_str()
        .expect("reservation id")
        .to_string()
}

fn active_reservation_count(runtime: &OrbitRuntime) -> usize {
    runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(
            &workspace_orbit_dir(runtime),
            workspace_task_reservation_id(runtime)
                .expect("workspace reservation id")
                .as_deref(),
        )
        .expect("list active")
        .reservations
        .len()
}

fn release_audit_payloads(runtime: &OrbitRuntime) -> Vec<Value> {
    runtime
        .list_audit_events(
            None,
            Some("orbit.task.locks.release".to_string()),
            Some(AuditEventStatus::Success),
            None,
            100,
        )
        .expect("list release audits")
        .into_iter()
        .filter(|event| event.command == "task.locks.reserve.released")
        .filter_map(|event| event.arguments_json)
        .map(|raw| serde_json::from_str(&raw).expect("parse audit json"))
        .collect()
}

#[cfg(unix)]
#[test]
fn doctor_repair_releases_only_rechecked_conclusive_stale_classes_and_is_idempotent() {
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");

    let fresh_task = create_context_task(&runtime, &repo_root, "fresh", "file:src/fresh.rs");
    let done_task = create_context_task_with_status(
        &runtime,
        &repo_root,
        "done",
        "file:src/done.rs",
        TaskStatus::Done,
    );
    let live_run = insert_running_run(&runtime, "live", std::process::id());
    let terminal_run = insert_running_run(&runtime, "terminal", std::process::id());
    runtime
        .stores()
        .jobs()
        .finalize_job_run(
            &terminal_run.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(1),
        )
        .expect("terminalize owner without cleanup");
    let orphan_run = insert_running_run(&runtime, "orphan", 999_999);

    let fresh = reserve_direct(&runtime, vec![fresh_task], "file:src/fresh.rs", None);
    let live = reserve_direct(
        &runtime,
        Vec::new(),
        "file:src/live.rs",
        Some(&live_run.run_id),
    );
    let ambiguous = reserve_direct(
        &runtime,
        vec!["ORB-missing".to_string()],
        "file:src/ambiguous.rs",
        None,
    );
    let absent = reserve_direct(
        &runtime,
        Vec::new(),
        "file:src/absent.rs",
        Some("jrun-absent"),
    );
    let terminal = reserve_direct(
        &runtime,
        Vec::new(),
        "file:src/terminal.rs",
        Some(&terminal_run.run_id),
    );
    let orphan = reserve_direct(
        &runtime,
        Vec::new(),
        "file:src/orphan.rs",
        Some(&orphan_run.run_id),
    );
    let done = reserve_direct(&runtime, vec![done_task], "file:src/done.rs", None);

    let diagnosed = runtime
        .list_stale_task_reservations()
        .expect("diagnose reservations");
    let diagnosed_ids = diagnosed
        .iter()
        .map(|candidate| candidate.reservation_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        diagnosed_ids,
        [&absent, &terminal, &orphan, &done]
            .into_iter()
            .map(String::as_str)
            .collect()
    );
    assert!(
        diagnosed
            .iter()
            .all(|candidate| !candidate.reason.is_empty()),
        "every stale class names its reason: {diagnosed:?}"
    );

    assert_eq!(
        runtime.release_stale_task_reservations().expect("repair"),
        4
    );
    assert_eq!(
        runtime
            .release_stale_task_reservations()
            .expect("idempotent repair"),
        0
    );

    let active = runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(
            &workspace_orbit_dir(&runtime),
            workspace_task_reservation_id(&runtime)
                .expect("workspace reservation id")
                .as_deref(),
        )
        .expect("list survivors");
    let survivor_ids = active
        .reservations
        .iter()
        .map(|reservation| reservation.reservation_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        survivor_ids,
        [&fresh, &live, &ambiguous]
            .into_iter()
            .map(String::as_str)
            .collect()
    );

    let payloads = release_audit_payloads(&runtime);
    for released_id in [&absent, &terminal, &orphan, &done] {
        assert!(payloads.iter().any(|payload| {
            payload["reservation_id"] == released_id.as_str()
                && payload["release_reason"] == "doctor_stale_task_lock"
        }));
    }
}

#[test]
fn task_reservation_gate_terminal_success_releases_owned_reservation_without_yaml_step() {
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), "").expect("write source");
    let task_id = create_context_task(&runtime, &repo_root, "gate", "file:src/lib.rs");
    let run = insert_running_run(&runtime, "task_gate_pipeline", std::process::id());
    let reservation_id = reserve_via_tool_for_owner(&runtime, &run.run_id, &task_id);
    assert_eq!(active_reservation_count(&runtime), 1);

    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize");

    assert_eq!(active_reservation_count(&runtime), 0);
    let payloads = release_audit_payloads(&runtime);
    assert!(payloads.iter().any(|payload| {
        payload["reservation_id"] == reservation_id
            && payload["owner_run_id"] == run.run_id
            && payload["release_reason"] == "run_terminal"
    }));
}

#[test]
fn task_reservation_child_terminal_does_not_release_parent_owned_reservation() {
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), "").expect("write source");
    let task_id = create_context_task(&runtime, &repo_root, "parent", "file:src/lib.rs");
    let parent = insert_running_run(&runtime, "task_gate_pipeline", std::process::id());
    let child = insert_running_run(&runtime, "agent_implement", std::process::id());
    reserve_via_tool_for_owner(&runtime, &parent.run_id, &task_id);

    runtime
        .finalize_job_run_with_reservation_cleanup(
            &child.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize child");

    let active = runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(
            &workspace_orbit_dir(&runtime),
            workspace_task_reservation_id(&runtime)
                .expect("workspace reservation id")
                .as_deref(),
        )
        .expect("list active");
    assert_eq!(active.reservations.len(), 1);
    assert_eq!(
        active.reservations[0].owner_run_id.as_deref(),
        Some(parent.run_id.as_str())
    );
}

#[test]
fn task_reservation_gate_terminal_failure_releases_owned_reservation() {
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), "").expect("write source");
    let task_id = create_context_task(&runtime, &repo_root, "failed", "file:src/lib.rs");
    let run = insert_running_run(&runtime, "task_gate_pipeline", std::process::id());
    reserve_via_tool_for_owner(&runtime, &run.run_id, &task_id);

    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Failed,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize failed gate");

    assert_eq!(active_reservation_count(&runtime), 0);
}

#[test]
fn task_reservation_unowned_manual_reservation_survives_unrelated_finalization() {
    let (_root, runtime, _repo_root) = test_runtime();
    let run = insert_running_run(&runtime, "unrelated", std::process::id());
    runtime
        .stores()
        .task_reservations()
        .reserve_task_reservation(TaskReservationReserveParams {
            workspace_orbit_dir: workspace_orbit_dir(&runtime),
            workspace_id: workspace_task_reservation_id(&runtime)
                .expect("workspace reservation id"),
            task_ids: vec!["T-manual".to_string()],
            requested_files: vec!["file:src/lib.rs".to_string()],
            actor: "manual".to_string(),
            ttl_seconds: 3600,
            owner_run_id: None,
            owner_metadata_json: None,
        })
        .expect("manual reserve");

    runtime
        .finalize_job_run_with_reservation_cleanup(
            &run.run_id,
            JobRunState::Success,
            Utc::now(),
            Some(1),
            TaskReservationReleaseReason::RunTerminal,
        )
        .expect("finalize unrelated");

    let active = runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(
            &workspace_orbit_dir(&runtime),
            workspace_task_reservation_id(&runtime)
                .expect("workspace reservation id")
                .as_deref(),
        )
        .expect("list active");
    assert_eq!(active.reservations.len(), 1);
    assert_eq!(active.reservations[0].owner_run_id, None);
}

#[cfg(unix)]
#[test]
fn task_reservation_reserve_pressure_reconciles_stale_running_owner() {
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src");
    std::fs::write(repo_root.join("src/lib.rs"), "").expect("write source");
    let stale_task = create_context_task(&runtime, &repo_root, "stale", "file:src/lib.rs");
    let waiter_task = create_context_task(&runtime, &repo_root, "waiter", "file:src/lib.rs");
    let stale_run = insert_running_run(&runtime, "task_gate_pipeline", 999_999);
    reserve_via_tool_for_owner(&runtime, &stale_run.run_id, &stale_task);

    let output = runtime
        .run_tool_with_context_and_role(
            "orbit.task.locks.reserve",
            json!({
                "task_ids": [waiter_task],
                "ttl_seconds": 3600,
                "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
            }),
            Role::Admin,
            ToolContext {
                session_context: operator_session(),
                ..ToolContext::default()
            },
        )
        .expect("reserve after pressure");

    assert_eq!(output["reserved"], true);
    let payloads = release_audit_payloads(&runtime);
    assert!(payloads.iter().any(|payload| {
        payload["owner_run_id"] == stale_run.run_id
            && payload["release_reason"] == "stale_run_reconciled"
    }));
}
