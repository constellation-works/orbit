//! Automatic batch preparation hydrates tasks only until its cap is filled.

use std::time::Duration;

use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_types::task::{TASK_DESCRIPTION_FILE_NAME, TaskStatus};
use serde_json::json;

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::batch_prepare::{BatchSpec, Qualification, prepare_batch};
use orbit_store::contracts::TaskListFilter;

#[test]
fn automatic_batch_stops_hydrating_once_max_tasks_are_selected() {
    if !enter_isolated_child(
        module_path!(),
        "automatic_batch_stops_hydrating_once_max_tasks_are_selected",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let add = |title: &str| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                description: "A candidate.".into(),
                acceptance_criteria: vec!["Selected within the cap.".into()],
                status: Some(TaskStatus::Proposed),
                ..Default::default()
            })
            .expect("add task")
            .id
    };
    let older = add("older");
    std::thread::sleep(Duration::from_millis(20));
    let newer = add("newer");

    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root()))
        .expect("task registry");
    let bundle = registry
        .canonical_task_bundle_path(&runtime.workspace_id().expect("workspace"), &newer)
        .expect("bundle");
    std::fs::remove_file(bundle.join(TASK_DESCRIPTION_FILE_NAME)).expect("drop the later body");

    let output = prepare_batch(
        &runtime,
        "test.prepare",
        &json!({"max_tasks": 1}),
        None,
        BatchSpec {
            claim: None,
            source_revision: None,
            source: json!({}),
            workspace_path: runtime.paths().repo_root.clone(),
            automatic_filter: TaskListFilter {
                statuses: Some(vec![TaskStatus::Proposed]),
                ..Default::default()
            },
            silent_reasons: &[],
            before_workspace: Vec::new(),
            before_task_count: Vec::new(),
        },
        |_| Ok(Default::default()),
        |_| true,
        |_task, _, _| Ok(Qualification::Selected(())),
        |task, ()| Ok((json!({"task_id": task.id}), "implementer".to_string())),
        |reason, _| reason.to_string(),
        |_, _| None,
    )
    .expect("tasks past the filled cap are not hydrated");

    assert_eq!(output["task_ids"], json!([older]));
    assert_eq!(
        output["excluded_by_reason"]["unexamined_after_max_tasks"],
        json!(1)
    );
    assert_eq!(output["mode"], json!("automatic"));
}
