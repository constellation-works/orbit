use super::super::index::{TaskLockIndex, task_lock_conflicts_indexed};
use crate::adapter::tool_host::test_support::{
    create_context_task, test_runtime, unmanaged_tool_env_guard,
};
use orbit_store::{TaskLockConflict, TaskLockHolder};
use orbit_types::task::TaskStatus;

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
