use orbit_store::{TaskLockConflict, TaskLockHolder};
use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::super::locks::{TaskLockIndex, task_lock_conflicts_indexed};
use crate::adapter::tool_host::test_support::{
    create_context_task, invalid_input_message, run_tool_as_operator, test_runtime,
    unmanaged_tool_env_guard,
};

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
        &TaskLockIndex::load(&runtime, &[]).expect("index task envelopes"),
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
