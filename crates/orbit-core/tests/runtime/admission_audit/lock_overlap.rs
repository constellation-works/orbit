use super::tool_support::{create_context_task, test_runtime, unmanaged_tool_env_guard};
use orbit_store::{TaskLockConflict, TaskLockHolder};
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
