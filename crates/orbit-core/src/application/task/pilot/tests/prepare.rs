//! Admissions that land between task-pilot selection and hydration.

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

use super::persist::{Workspace, workspace};
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

use super::super::prepare::hydration_test_hook;

fn add_task(runtime: &OrbitRuntime, title: &str, status: TaskStatus) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("{title} description"),
            acceptance_criteria: vec![format!("{title} is done")],
            plan: format!("{title} plan"),
            status: Some(status),
            ..Default::default()
        })
        .expect("add task")
}

fn run(workspace: &Workspace, action: &str, input: Value) -> Value {
    workspace
        .runtime
        .run_deterministic(action, &json!({}), &input, ToolContext::default())
        .unwrap_or_else(|error| panic!("{action}: {error}"))
}

/// Prepare with `task_ids` (empty for automatic discovery) while the hook
/// lands `between` after selection and before hydration.
fn prepare_racing(
    workspace: &Workspace,
    task_ids: &[&str],
    between: impl FnOnce(&OrbitRuntime) + 'static,
) -> Value {
    hydration_test_hook::install(between);
    let path = workspace.repo.canonicalize().expect("canonicalize repo");
    run(
        workspace,
        "prepare_task_pilot",
        json!({"workspace_path": path, "task_ids": task_ids, "base_branch": "main"}),
    )
}

/// Apply one assessment per prepared task, partition by partition.
fn apply(workspace: &Workspace, prepared: &Value) -> Value {
    let results = prepared["partitions"]
        .as_array()
        .expect("partitions")
        .iter()
        .map(|partition| {
            let tasks = prepared["tasks"]
                .as_array()
                .expect("tasks")
                .iter()
                .filter(|task| {
                    partition["task_ids"]
                        .as_array()
                        .expect("partition task ids")
                        .contains(&task["task_id"])
                })
                .map(|task| {
                    json!({
                        "task_id": task["task_id"],
                        "context_files_before": task["context_files_before"],
                        "context_files_after": ["file:README.md"],
                        "disposition": "selectors",
                        "recommended_crew": "opus",
                        "recommended_complexity": "low",
                        "assessment_rationale": "README.md is the assessed selector.",
                        "validation_approach": "Run the prepare and apply actions.",
                        "confidence": "high",
                        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
                        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
                        "duplicate_of": Value::Null, "already_landed": Value::Null,
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "partition_index": partition["partition_index"],
                "task_ids": partition["task_ids"],
                "tasks": tasks,
            })
        })
        .collect::<Vec<_>>();
    run(
        workspace,
        "apply_task_pilot_results",
        json!({
            "workspace_path": prepared["workspace_path"],
            "prepared": prepared,
            "results": results,
        }),
    )
}

fn admit(runtime: &OrbitRuntime, task_id: &str) {
    runtime
        .admit_task_for_workflow(task_id, "worktree_setup")
        .expect("admit task");
}

fn outcome<'a>(output: &'a Value, task_id: &str) -> &'a Value {
    output["task_outcomes"]
        .as_array()
        .expect("task outcomes")
        .iter()
        .find(|outcome| outcome["task_id"] == task_id)
        .unwrap_or_else(|| panic!("no outcome for {task_id}: {output}"))
}

fn assert_not_piloted(workspace: &Workspace, task_id: &str) {
    let task = workspace.runtime.get_task(task_id).expect("task");
    assert_eq!(task.status, TaskStatus::InProgress);
    assert!(task.context_files.is_empty(), "{:?}", task.context_files);
    assert!(
        workspace
            .runtime
            .get_task_history(task_id)
            .expect("history")
            .iter()
            .all(|entry| entry.event != "task_pilot_applied"),
        "apply wrote the admitted task"
    );
}

#[test]
fn automatic_admission_between_selection_and_hydration_settles_superseded() {
    let workspace = workspace(None);
    let admitted = add_task(&workspace.runtime, "admitted", TaskStatus::Backlog);
    let promoted = add_task(&workspace.runtime, "promoted", TaskStatus::Proposed);
    let (admitted_id, promoted_id) = (admitted.id.clone(), promoted.id.clone());
    let prepared = prepare_racing(&workspace, &[], move |runtime| {
        admit(runtime, &admitted_id);
        runtime
            .update_task(
                &promoted_id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
            )
            .expect("promote");
    });
    // The admitted task leaves the pilot; the promoted one is still selected
    // and is assessed against its status after the promotion.
    assert_eq!(prepared["task_ids"], json!([promoted.id]), "{prepared}");
    assert_eq!(prepared["tasks"][0]["status"], "backlog", "{prepared}");

    let output = apply(&workspace, &prepared);
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["outcome"], "superseded", "{output}");
    assert_eq!(output["superseded_count"], 1, "{output}");
    assert_eq!(output["applied_count"], 1, "{output}");
    assert_eq!(output["unresolved_count"], 0, "{output}");
    assert!(output["error"].is_null(), "{output}");
    assert_eq!(outcome(&output, &admitted.id)["reason"], "status_changed");
    assert_eq!(
        run(
            &workspace,
            "pipeline_success_guard",
            json!({"result": output})
        )["succeeded"],
        true
    );
    assert_not_piloted(&workspace, &admitted.id);
    assert_eq!(
        workspace
            .runtime
            .get_task(&promoted.id)
            .expect("promoted")
            .context_files,
        ["file:README.md"]
    );
}

#[test]
fn explicit_admission_between_selection_and_hydration_settles_superseded() {
    let workspace = workspace(None);
    let task = add_task(&workspace.runtime, "admitted", TaskStatus::Backlog);
    let task_id = task.id.clone();
    let prepared = prepare_racing(&workspace, &[&task.id], move |runtime| {
        admit(runtime, &task_id);
    });
    assert_eq!(prepared["task_ids"], json!([task.id]), "{prepared}");
    assert_eq!(prepared["tasks"][0]["status"], "backlog", "{prepared}");

    let output = apply(&workspace, &prepared);
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["outcome"], "superseded", "{output}");
    assert_eq!(output["superseded_count"], 1, "{output}");
    assert_eq!(output["unresolved_count"], 0, "{output}");
    assert_eq!(outcome(&output, &task.id)["outcome"], "superseded");
    assert_not_piloted(&workspace, &task.id);
}
