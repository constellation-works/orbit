use orbit_common::OrbitError;
use orbit_store::{TaskLockConflict, TaskLockHolder};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::application::task::TaskAddParams;

use super::super::locks::{
    MAX_TASK_RESERVATION_TTL_SECONDS, TaskLockIndex, TaskLockReservationScope,
    parse_task_lock_reservation_scope, requested_task_files_indexed, task_lock_conflicts_indexed,
};
use crate::adapter::tool_host::test_support::{
    create_context_task, invalid_input_message, run_tool_as_operator, test_runtime,
    unmanaged_tool_env_guard,
};

fn v2_test_runtime() -> (TempDir, OrbitRuntime, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("create tempdir");
    let global_root = root.path().join("global");
    let repo_root = root.path().join("repo");
    let workspace_root = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("create global root");
    std::fs::create_dir_all(&workspace_root).expect("create workspace root");
    let runtime =
        OrbitRuntime::from_roots(&global_root, &workspace_root).expect("build v2 runtime");
    (root, runtime, repo_root)
}

#[test]
fn parse_task_lock_reservation_scope_requires_exactly_one_shape() {
    let _env = unmanaged_tool_env_guard();
    let missing = invalid_input_message(parse_task_lock_reservation_scope(&json!({})));
    assert!(missing.contains("exactly one of 'task_ids' or 'files' must be provided"));

    let both = invalid_input_message(parse_task_lock_reservation_scope(&json!({
        "task_ids": ["T20260506-15"],
        "files": ["file:src/lib.rs"],
    })));
    assert!(both.contains("exactly one of 'task_ids' or 'files' must be provided"));
}

#[test]
fn parse_task_lock_reservation_scope_validates_file_selectors() {
    let _env = unmanaged_tool_env_guard();
    let scope = parse_task_lock_reservation_scope(&json!({
        "files": ["file:src/../src/lib.rs", "dir:src/auth/"],
    }))
    .expect("parse files shape");
    assert_eq!(
        scope,
        TaskLockReservationScope::Files(vec![
            "dir:src/auth".to_string(),
            "file:src/lib.rs".to_string(),
        ])
    );

    let raw_path = invalid_input_message(parse_task_lock_reservation_scope(&json!({
        "files": ["src/lib.rs"],
    })));
    assert!(raw_path.contains("`file:`"));
    assert!(raw_path.contains("`dir:`"));

    let symbol = invalid_input_message(parse_task_lock_reservation_scope(&json!({
        "files": ["symbol:src/lib.rs#run:function"],
    })));
    assert!(symbol.contains("`file:`"));
    assert!(symbol.contains("`dir:`"));
    assert!(symbol.contains("selectors are not supported for task locks"));

    let module = invalid_input_message(parse_task_lock_reservation_scope(&json!({
        "files": ["module:orbit_core::scheduler"],
    })));
    assert!(module.contains("selectors are not supported for task locks"));

    let command = invalid_input_message(parse_task_lock_reservation_scope(&json!({
        "files": ["command:task.update"],
    })));
    assert!(command.contains("selectors are not supported for task locks"));
}

#[test]
fn task_lock_reservation_ttl_accepts_its_finite_delivery_limit_only() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo_root) = test_runtime();

    let reserved = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": ["file:src/lib.rs"],
            "ttl_seconds": MAX_TASK_RESERVATION_TTL_SECONDS,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("reserve at the supported delivery limit");
    assert_eq!(reserved["reserved"], true);

    let error = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": ["file:src/other.rs"],
            "ttl_seconds": MAX_TASK_RESERVATION_TTL_SECONDS + 1,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    ));
    assert!(
        error.contains(&MAX_TASK_RESERVATION_TTL_SECONDS.to_string()),
        "{error}"
    );
}

#[test]
fn task_locks_reserve_adapter_surfaces_new_validation_errors() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo_root) = test_runtime();

    let missing = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({ "model": orbit_common::test_fixtures::TEST_CODEX_MODEL }),
    ));
    assert!(missing.contains("exactly one of 'task_ids' or 'files' must be provided"));

    let both = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": ["T20260506-15"],
            "files": ["file:src/lib.rs"],
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    ));
    assert!(both.contains("exactly one of 'task_ids' or 'files' must be provided"));

    let raw_path = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": ["src/lib.rs"],
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    ));
    assert!(raw_path.contains("`file:`"));
    assert!(raw_path.contains("`dir:`"));

    let symbol = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": ["symbol:src/lib.rs#run:function"],
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    ));
    assert!(symbol.contains("`file:`"));
    assert!(symbol.contains("`dir:`"));
    assert!(symbol.contains("selectors are not supported for task locks"));
}

#[test]
fn requested_task_files_prune_missing_context_entries() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("docs/design")).expect("create docs dir");
    std::fs::write(repo_root.join("docs/design/groundhog.md"), "alias").expect("write alias doc");

    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["docs/design/groundhog.md", "docs/design/missing.md"],
    );

    let index = TaskLockIndex::load(&runtime).expect("index tasks");
    let requested =
        requested_task_files_indexed(&index, &[task.id], runtime.paths().repo_root.as_path())
            .expect("collect requested task files");
    assert_eq!(requested, vec!["file:docs/design/groundhog.md".to_string()]);
}

#[test]
fn active_epic_root_holds_union_of_descendant_context_files() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    for path in ["src/root.rs", "src/one.rs", "src/two.rs"] {
        let full_path = repo_root.join(path);
        std::fs::create_dir_all(full_path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        std::fs::write(full_path, "fixture\n").expect("write fixture");
    }
    let epic = runtime
        .add_task(TaskAddParams {
            title: "Epic root".to_string(),
            description: "Epic fixture".to_string(),
            acceptance_criteria: vec!["assembled".to_string()],
            tags: vec!["epic".to_string()],
            plan: "drain children".to_string(),
            context_files: vec!["file:src/root.rs".to_string()],
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("create epic");
    for (title, path) in [("one", "src/one.rs"), ("two", "src/two.rs")] {
        runtime
            .add_task(TaskAddParams {
                parent_id: Some(epic.id.clone()),
                title: title.to_string(),
                description: "Child fixture".to_string(),
                acceptance_criteria: vec!["done".to_string()],
                plan: "implement".to_string(),
                context_files: vec![format!("file:{path}")],
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .expect("create epic child");
    }

    assert_eq!(
        requested_task_files_indexed(
            &TaskLockIndex::load(&runtime).expect("index tasks"),
            std::slice::from_ref(&epic.id),
            runtime.paths().repo_root.as_path()
        )
        .expect("collect epic lock surface"),
        vec![
            "file:src/one.rs".to_string(),
            "file:src/root.rs".to_string(),
            "file:src/two.rs".to_string(),
        ]
    );
    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list task locks");
    let epic_lock = locks["by_task"]
        .as_array()
        .expect("task locks")
        .iter()
        .find(|entry| entry["id"] == epic.id)
        .expect("epic lock entry");
    assert_eq!(
        epic_lock["context_files"],
        json!(["file:src/one.rs", "file:src/root.rs", "file:src/two.rs"])
    );
}

#[test]
fn task_lock_conflicts_ignore_missing_held_context_entries() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let holder = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["docs/design/groundhog.md", "src/lib.rs"],
    );

    let conflicts = task_lock_conflicts_indexed(
        &TaskLockIndex::load(&runtime).expect("index tasks"),
        &[],
        &[
            "docs/design/groundhog.md".to_string(),
            "src/lib.rs".to_string(),
        ],
        runtime.paths().repo_root.as_path(),
    );

    assert_eq!(
        conflicts,
        vec![TaskLockConflict {
            file: "src/lib.rs".to_string(),
            held_by: TaskLockHolder::Task,
            held_by_id: holder.id,
        }]
    );
}

#[test]
fn task_lock_conflicts_use_selector_anchor_overlap() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let holder = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["symbol:src/lib.rs#ok:function"],
    );

    let conflicts = task_lock_conflicts_indexed(
        &TaskLockIndex::load(&runtime).expect("index tasks"),
        &[],
        &["file:src/lib.rs".to_string(), "dir:src".to_string()],
        runtime.paths().repo_root.as_path(),
    );

    assert_eq!(
        conflicts,
        vec![
            TaskLockConflict {
                file: "dir:src".to_string(),
                held_by: TaskLockHolder::Task,
                held_by_id: holder.id.clone(),
            },
            TaskLockConflict {
                file: "file:src/lib.rs".to_string(),
                held_by: TaskLockHolder::Task,
                held_by_id: holder.id,
            },
        ]
    );
}

#[test]
fn task_scope_reserve_refuses_a_task_with_no_context_surface() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();

    let task = create_context_task(&runtime, &repo_root, TaskStatus::Backlog, &[]);

    let message = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [task.id.clone()],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    ));
    assert!(message.contains("no context surface"), "{message}");
    assert!(message.contains(&task.id), "{message}");

    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list task locks");
    assert_eq!(locks["total_reservations"], 0);
}

/// A declared selector for a not-yet-created file is a different situation
/// than declaring nothing at all: the domain already tolerates it elsewhere
/// (pruned from the lock surface), so a task-scope reservation must still
/// admit it rather than folding it into the "no context surface" refusal.
#[test]
fn task_scope_reserve_still_admits_a_task_whose_declared_file_does_not_exist_yet() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();

    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["docs/design/missing.md"],
    );

    let reserved = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [task.id],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("a declared-but-not-yet-created file must still reserve");
    assert_eq!(reserved["reserved"], true);
    assert_eq!(reserved["reserved_files"], json!([]));
}

#[test]
fn reservation_conflicts_clear_immediately_after_release() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let first = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );
    let second = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );

    let first_reserve = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [first.id.clone()],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("reserve first task");
    let reservation_id = first_reserve
        .get("reservation_id")
        .and_then(Value::as_str)
        .expect("reservation id is present")
        .to_string();

    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list locks");
    assert_eq!(locks["total_reservations"], 1);
    assert_eq!(
        locks["by_reservation"][0]["reservation_id"],
        reservation_id.as_str()
    );
    assert_eq!(locks["by_reservation"][0]["task_ids"], json!([first.id]));
    assert_eq!(
        locks["by_reservation"][0]["files"],
        json!(["file:src/lib.rs"])
    );
    assert!(
        locks["by_reservation"][0]["expires_at"].is_string(),
        "reservation visibility should include expiration"
    );

    let blocked = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [second.id.clone()],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("second reservation returns conflict");
    assert_eq!(blocked["reserved"], false);
    assert_eq!(
        blocked["conflicts"],
        json!([{
            "file": "file:src/lib.rs",
            "held_by": "reservation",
            "held_by_id": reservation_id.clone(),
        }])
    );

    let release = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.release",
        json!({
            "reservation_id": reservation_id,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("release reservation");
    assert_eq!(release["released"], true);

    let second_reserve = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [second.id],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("second reservation succeeds after release");
    assert_eq!(second_reserve["reserved"], true);
}

#[test]
fn release_rejects_an_identifier_of_the_wrong_form_instead_of_a_falsy_no_op() {
    // ORB-10651: reservation ids are minted as `reservation-<nanos>`. Passing
    // a task id (or any other identifier shape) must not fall through to the
    // "no matching row" path, which reads as a completed release.
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo_root) = test_runtime();

    let message = invalid_input_message(run_tool_as_operator(
        &runtime,
        "orbit.task.locks.release",
        json!({
            "reservation_id": "ORB-10651",
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    ));
    assert!(message.contains("reservation_id"), "{message}");
    assert!(message.contains("reservation-"), "{message}");
    assert!(message.contains("ORB-10651"), "{message}");
}

#[test]
fn v2_task_locks_store_workspace_binding_id() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = v2_test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );
    assert_eq!(task.id, "ORB-00000");

    let reservation = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [task.id],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("reserve v2 task");
    assert_eq!(reservation["reserved"], true);

    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list locks");
    let workspace_id = locks["by_reservation"][0]["workspace_id"]
        .as_str()
        .expect("reservation carries workspace_id");
    assert!(workspace_id.starts_with("repo-"), "{workspace_id}");
}

#[test]
fn v2_task_locks_fail_when_workspace_binding_config_disappears() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = v2_test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");
    std::fs::remove_file(repo_root.join(".orbit/config.yaml")).expect("remove workspace config");

    let err = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": ["file:src/lib.rs"],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect_err("missing v2 binding config should fail");
    assert!(matches!(
        err,
        OrbitError::Store(message)
            if message.contains("task artifact workspace config is missing")
    ));
}

#[test]
fn files_shape_reservations_conflict_and_release_like_task_reservations() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let direct_reserve = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": [
                format!("file:{}", repo_root.join("src/lib.rs").display()),
                "dir:src/auth/",
            ],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("reserve direct file selectors");
    assert_eq!(direct_reserve["reserved"], true);
    assert_eq!(
        direct_reserve["reserved_files"],
        json!(["dir:src/auth", "file:src/lib.rs"])
    );
    let reservation_id = direct_reserve
        .get("reservation_id")
        .and_then(Value::as_str)
        .expect("reservation id is present")
        .to_string();

    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list locks");
    assert_eq!(locks["total_reservations"], 1);
    assert_eq!(
        locks["by_reservation"][0]["reservation_id"],
        reservation_id.as_str()
    );
    assert_eq!(locks["by_reservation"][0]["task_ids"], json!([]));
    assert_eq!(
        locks["by_reservation"][0]["files"],
        json!(["dir:src/auth", "file:src/lib.rs"])
    );

    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );
    let blocked = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [task.id.clone()],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("task reservation returns conflict");
    assert_eq!(blocked["reserved"], false);
    assert_eq!(
        blocked["conflicts"],
        json!([{
            "file": "file:src/lib.rs",
            "held_by": "reservation",
            "held_by_id": reservation_id.clone(),
        }])
    );

    let release = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.release",
        json!({
            "reservation_id": reservation_id,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("release direct reservation");
    assert_eq!(release["released"], true);

    let task_reserve = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [task.id],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("task reservation succeeds after release");
    assert_eq!(task_reserve["reserved"], true);
}

#[test]
fn files_shape_reservations_reject_outside_workspace_before_persisting() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, _repo_root) = test_runtime();

    let error = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "files": ["file:/outside-workspace/lib.rs"],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect_err("outside-workspace selector is rejected");
    let message = invalid_input_message::<Value>(Err(error));
    assert!(
        message.contains("must remain inside workspace"),
        "{message}"
    );

    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list task locks");
    assert_eq!(locks["total_reservations"], 0);
}

use orbit_tools::{ReservationOwnerContext, ToolContext};
use orbit_types::policy::Role;
use orbit_types::telemetry::AuditEvent;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use std::collections::BTreeSet;

fn task_lock_audit_event(runtime: &OrbitRuntime, tool_name: &str, command: &str) -> AuditEvent {
    runtime
        .list_audit_events(None, Some(tool_name.to_string()), None, None, 16)
        .expect("list audit events")
        .into_iter()
        .find(|event| event.command == command)
        .expect("task lock audit event")
}

fn reserve_files(runtime: &OrbitRuntime, owner_run_id: Option<&str>) -> String {
    let input = json!({
        "files": ["file:src/lib.rs"],
        "ttl_seconds": 3600,
        "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
    });
    let operator_session = ToolSessionContext {
        effective_capabilities: BTreeSet::from([McpCapability::Operator]),
        ..ToolSessionContext::default()
    };
    let output = match owner_run_id {
        Some(owner_run_id) => runtime
            .run_tool_with_context_and_role(
                "orbit.task.locks.reserve",
                input,
                Role::Admin,
                ToolContext {
                    session_context: operator_session,
                    reservation_owner: Some(ReservationOwnerContext {
                        owner_run_id: owner_run_id.to_string(),
                        owner_metadata_json: None,
                    }),
                    ..ToolContext::default()
                },
            )
            .expect("reserve direct file selectors with owner"),
        None => runtime
            .run_tool_with_context_and_role(
                "orbit.task.locks.reserve",
                input,
                Role::Admin,
                ToolContext {
                    session_context: operator_session,
                    ..ToolContext::default()
                },
            )
            .expect("reserve direct file selectors"),
    };

    output
        .get("reservation_id")
        .and_then(Value::as_str)
        .expect("reservation id")
        .to_string()
}

#[test]
fn release_audit_without_owner_has_no_task_or_job_run_id() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let reservation_id = reserve_files(&runtime, None);
    let release = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.release",
        json!({
            "reservation_id": reservation_id.clone(),
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("release reservation");
    assert_eq!(release["released"], true);

    let row = task_lock_audit_event(
        &runtime,
        "orbit.task.locks.release",
        "task.locks.reserve.released",
    );
    assert_eq!(row.target_id.as_deref(), Some(reservation_id.as_str()));
    assert!(row.task_id.is_none());
    assert!(row.job_run_id.is_none());
}

#[test]
fn release_audit_uses_reservation_owner_run_id() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let reservation_id = reserve_files(&runtime, Some("jrun-owner"));
    let release = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.release",
        json!({
            "reservation_id": reservation_id.clone(),
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("release reservation");
    assert_eq!(release["released"], true);

    let row = task_lock_audit_event(
        &runtime,
        "orbit.task.locks.release",
        "task.locks.reserve.released",
    );
    assert_eq!(row.target_id.as_deref(), Some(reservation_id.as_str()));
    assert!(row.task_id.is_none());
    assert_eq!(row.job_run_id.as_deref(), Some("jrun-owner"));
}

#[test]
fn reserve_audit_for_task_scope_records_first_task_id() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");
    let task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );

    let reserve = run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [task.id.clone()],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("reserve task scope");
    assert_eq!(reserve["reserved"], true);
    let reservation_id = reserve["reservation_id"]
        .as_str()
        .expect("reservation id")
        .to_string();

    let row = task_lock_audit_event(
        &runtime,
        "orbit.task.locks.reserve",
        "task.locks.reserve.granted",
    );
    assert_eq!(row.target_id.as_deref(), Some(reservation_id.as_str()));
    assert_eq!(row.task_id.as_deref(), Some(task.id.as_str()));
    let payload: Value = serde_json::from_str(row.arguments_json.as_deref().expect("payload"))
        .expect("parse reservation audit payload");
    assert_eq!(payload["actor"], json!("codex"));
}

/// ORB-12251: a caller could `reserve` with no capability at all and then be
/// refused `release` on the very reservation it just created — an `unknown`
/// caller could gate dispatch admission for a surface it was not itself
/// trusted to clear. `reserve` must deny the same unprivileged caller
/// `release` already denies, and admit the same `operator`/`runner` callers.
#[test]
fn reserve_requires_the_same_capability_as_release() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");

    let unprivileged = ToolContext::default();
    let reserve_input = json!({
        "files": ["file:src/lib.rs"],
        "ttl_seconds": 3600,
        "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
    });

    let denied_reserve = runtime
        .run_tool_with_context_and_role(
            "orbit.task.locks.reserve",
            reserve_input.clone(),
            Role::Admin,
            unprivileged.clone(),
        )
        .expect_err("an unprivileged caller must not be able to reserve");
    let OrbitError::CapabilityDenied(reserve_message) = denied_reserve else {
        panic!("expected capability denial, got {denied_reserve:?}");
    };
    assert!(reserve_message.contains("orbit.task.locks.reserve"));
    assert!(reserve_message.contains("operator or runner"));

    let denied_release = runtime
        .run_tool_with_context_and_role(
            "orbit.task.locks.release",
            json!({
                "reservation_id": "reservation-does-not-matter",
                "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
            }),
            Role::Admin,
            unprivileged,
        )
        .expect_err("an unprivileged caller must not be able to release");
    let OrbitError::CapabilityDenied(release_message) = denied_release else {
        panic!("expected capability denial, got {denied_release:?}");
    };
    assert!(release_message.contains("orbit.task.locks.release"));
    assert!(release_message.contains("operator or runner"));

    // The same caller that was refused above is admitted for both once it
    // holds either half of the shared `operator or runner` requirement.
    for capability in [McpCapability::Operator, McpCapability::Runner] {
        let context = ToolContext {
            session_context: ToolSessionContext {
                effective_capabilities: BTreeSet::from([capability]),
                ..ToolSessionContext::default()
            },
            ..ToolContext::default()
        };
        let reserved = runtime
            .run_tool_with_context_and_role(
                "orbit.task.locks.reserve",
                reserve_input.clone(),
                Role::Admin,
                context.clone(),
            )
            .unwrap_or_else(|error| panic!("{capability} must be able to reserve: {error}"));
        assert_eq!(reserved["reserved"], true);
        let reservation_id = reserved["reservation_id"]
            .as_str()
            .expect("reservation id")
            .to_string();

        let released = runtime
            .run_tool_with_context_and_role(
                "orbit.task.locks.release",
                json!({
                    "reservation_id": reservation_id,
                    "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
                }),
                Role::Admin,
                context,
            )
            .unwrap_or_else(|error| panic!("{capability} must be able to release: {error}"));
        assert_eq!(released["released"], true);
    }
}

/// ORB-12251: `locks list` reported "0 task(s)" for a reservation that named
/// a task ID, because the count only walked active (in-progress/review)
/// tasks. A task-bound reservation's `task_ids` must count too, even for a
/// task that is not itself active — and a task counted both ways must not be
/// double-counted.
#[test]
fn locks_list_counts_distinct_tasks_across_reservations_and_active_tasks() {
    let _env = unmanaged_tool_env_guard();
    let (_root, runtime, repo_root) = test_runtime();
    std::fs::create_dir_all(repo_root.join("src")).expect("create src dir");
    std::fs::write(repo_root.join("src/lib.rs"), "pub fn ok() {}\n").expect("write source file");
    std::fs::write(repo_root.join("other.rs"), "pub fn other() {}\n")
        .expect("write second source file");

    // A backlog task reserved by ID: not active, so it contributes to the
    // count only through the reservation's `task_ids`.
    let backlog_task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::Backlog,
        &["file:src/lib.rs"],
    );
    run_tool_as_operator(
        &runtime,
        "orbit.task.locks.reserve",
        json!({
            "task_ids": [backlog_task.id],
            "ttl_seconds": 3600,
            "model": orbit_common::test_fixtures::TEST_CODEX_MODEL,
        }),
    )
    .expect("reserve backlog task by id");

    // An active task holding a different file, counted through `by_task`.
    let active_task = create_context_task(
        &runtime,
        &repo_root,
        TaskStatus::InProgress,
        &["file:other.rs"],
    );

    let locks = runtime
        .run_tool("orbit.task.locks", json!({}))
        .expect("list task locks");
    assert_eq!(
        locks["total_tasks"],
        json!(2),
        "expected the backlog task named by the reservation and the active task, got {locks}"
    );
    assert_eq!(locks["by_task"].as_array().expect("by_task array").len(), 1);
    assert_eq!(locks["by_task"][0]["id"], json!(active_task.id));
}
