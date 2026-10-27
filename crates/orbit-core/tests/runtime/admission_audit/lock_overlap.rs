use super::tool_support::{create_context_task, test_runtime, unmanaged_tool_env_guard};
use orbit_store::contracts::{
    LeaseInsertParams, LeaseMode, LeasePrincipalClass, LeasePriority, LeaseReleaseParams,
    StoreRetentionBackend,
};
use orbit_store::{TaskLockConflict, TaskLockHolder, TaskReservationReleaseReason};
use orbit_types::task::TaskStatus;

#[test]
fn task_lock_conflicts_use_selector_anchor_overlap() {
    if !super::isolate(
        module_path!(),
        "task_lock_conflicts_use_selector_anchor_overlap",
    ) {
        return;
    }

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

    let conflicts = super::tool_support::run_tool_as_operator(&runtime, "orbit.task.locks.reserve", serde_json::json!({"files": ["file:src/lib.rs", "dir:src"], "ttl_seconds": 3600, "model": "codex"})).unwrap()["conflicts"].clone();

    assert_eq!(
        conflicts,
        serde_json::to_value(vec![
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
        ])
        .unwrap()
    );
}

#[test]
fn task_scope_reserve_is_refused_by_a_hard_lease_and_granted_past_an_advisory_one() {
    if !super::isolate(
        module_path!(),
        "task_scope_reserve_is_refused_by_a_hard_lease_and_granted_past_an_advisory_one",
    ) {
        return;
    }

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
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let orbit_dir = runtime.paths().orbit_dir.to_string_lossy().into_owned();
    let leases = runtime.stores().task_reservations();
    let lease = |mode| {
        leases
            .insert_lease(LeaseInsertParams {
                workspace_orbit_dir: orbit_dir.clone(),
                workspace_id: Some(workspace_id.clone()),
                selectors: vec!["dir:src".into()],
                mode,
                priority: LeasePriority::Normal,
                tags: Vec::new(),
                principal: "alice".into(),
                principal_class: LeasePrincipalClass::Human,
                machine_id: None,
                base_branch: None,
                bound_branch: None,
                actor: "alice".into(),
                ttl_seconds: 600,
                token_id: None,
                wait: false,
                task_conflicts: Vec::new(),
                aging_minutes: 30,
            })
            .expect("insert lease")
            .lease
            .expect("lease recorded")
    };
    let reserve = || {
        super::tool_support::run_tool_as_operator(
            &runtime,
            "orbit.task.locks.reserve",
            serde_json::json!({"task_ids": [task.id], "ttl_seconds": 600, "model": "codex"}),
        )
        .expect("reserve")
    };

    let hard = lease(LeaseMode::Hard);
    let denied = reserve();
    assert_eq!(denied["reserved"], false, "{denied}");
    let expected = serde_json::to_value(vec![TaskLockConflict {
        file: "file:src/lib.rs".to_string(),
        held_by: TaskLockHolder::Lease,
        held_by_id: hard.lease_id.clone(),
    }])
    .unwrap();
    assert_eq!(denied["conflicts"], expected);
    let denials = runtime
        .sqlite_store()
        .expect("store")
        .list_task_lock_denials(&workspace_id)
        .expect("denial log");
    assert_eq!(denials.len(), 1, "the refusal is in the denial log");
    let logged: serde_json::Value = serde_json::from_str(&denials[0].arguments_json).unwrap();
    assert_eq!(logged["conflicts"], expected);

    leases
        .release_lease(LeaseReleaseParams {
            workspace_orbit_dir: orbit_dir.clone(),
            workspace_id: Some(workspace_id.clone()),
            lease_id: hard.lease_id,
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .expect("release lease");
    lease(LeaseMode::Advisory);
    let granted = reserve();
    assert_eq!(granted["reserved"], true, "{granted}");
    assert!(
        granted["reservation_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("reservation-")),
        "{granted}"
    );
}
