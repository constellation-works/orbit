//! Explainable task-pilot complexity assessment and persistence fixtures.

use orbit_types::task::{Task, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use serde_json::{Value, json};

use super::super::task_pilot::{apply, member_ready, prepare};
use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_layout, write_workspace_file,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn seed_task(runtime: &OrbitRuntime, title: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The fixture outcome is observable.".to_string()],
            plan: "Inspect and update the fixture.".to_string(),
            workspace_path: Some(".".to_string()),
            priority: TaskPriority::Medium,
            complexity: TaskComplexity::Unassessed,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed assessment task")
}

fn prepare_task(runtime: &OrbitRuntime, repo_root: &std::path::Path, task: &Task) -> Value {
    prepare(
        runtime,
        "prepare_task_pilot",
        &json!({"task_ids": [task.id], "workspace_path": repo_root}),
    )
    .expect("prepare assessment task")
}

fn assessment(task: &Task, selector: &str, complexity: &str) -> Value {
    json!({
        "task_id": task.id,
        "context_files_before": task.context_files,
        "context_files_after": [selector],
        "disposition": "selectors",
        "recommended_crew": "luna",
        "recommended_complexity": complexity,
        "assessment_rationale": "The repair certainty, behavioral change, coupling, and validation boundary support this rating.",
        "confidence": "high",
        "evidence_gaps": [],
        "validation_approach": "Run the focused behavioral checks.",
        "reassessment_triggers": ["the owning boundary changes"],
        "blocked_by": [],
        "duplicate_of": null,
        "already_landed": null,
        "release_action_required": null,
        "adr_conflicts": [],
        "utility_warnings": [],
        "surface_warnings": [],
    })
}

fn apply_assessment(
    runtime: &OrbitRuntime,
    repo_root: &std::path::Path,
    prepared: Value,
    task: &Task,
    assessment: Value,
) -> Value {
    apply(
        runtime,
        "apply_task_pilot_results",
        &json!({
            "prepared": prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": [task.id],
                "tasks": [assessment],
                "summary": "assessment fixture",
            }],
            "workspace_path": repo_root,
        }),
    )
    .expect("apply assessment")
}

#[test]
fn representative_repairs_persist_complexity_independently_of_urgency() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    let cases = [
        ("critical compatible bump", "src/dependency.lock", "low"),
        ("caller migration", "src/caller.rs", "medium"),
        ("trust-boundary redesign", "src/trust.rs", "hard"),
    ];

    for (title, path, complexity) in cases {
        write_workspace_file(&repo_root, path);
        let task = seed_task(&runtime, title);
        if title.starts_with("critical") {
            runtime
                .update_task(
                    &task.id,
                    TaskUpdateParams {
                        priority: Some(TaskPriority::Critical),
                        ..TaskUpdateParams::default()
                    },
                )
                .expect("raise fixture urgency");
        }
        let task = runtime.get_task(&task.id).expect("reload assessment task");
        let prepared = prepare_task(&runtime, &repo_root, &task);
        let output = apply_assessment(
            &runtime,
            &repo_root,
            prepared,
            &task,
            assessment(&task, &format!("file:{path}"), complexity),
        );

        assert_eq!(output["status"], "succeeded");
        assert_eq!(
            runtime
                .get_task(&task.id)
                .expect("assessed task")
                .complexity,
            Some(complexity.parse::<TaskComplexity>().expect("complexity"))
        );
    }
}

#[test]
fn missing_evidence_stays_unassessed_with_an_actionable_preparation_outcome() {
    let (_root, runtime, repo_root) = runtime_with_workspace_layout();
    write_workspace_file(&repo_root, "src/investigate.rs");
    let task = seed_task(&runtime, "uncertain repair");
    let prepared = prepare_task(&runtime, &repo_root, &task);
    let mut result = assessment(&task, "file:src/investigate.rs", "unassessed");
    result["confidence"] = json!("low");
    result["assessment_rationale"] =
        json!("The failing boundary is known, but the owning caller has not been identified.");
    result["evidence_gaps"] = json!(["Trace the caller that supplies the untrusted value."]);
    result["validation_approach"] =
        json!("Inspect callers, then rerun task-pilot with the discovered owner.");

    let output = apply_assessment(&runtime, &repo_root, prepared, &task, result);

    assert_eq!(output["status"], "succeeded");
    assert!(!member_ready(&output["tasks"][0]));
    let current = runtime.get_task(&task.id).expect("unassessed task");
    assert_eq!(current.complexity, Some(TaskComplexity::Unassessed));
    assert_eq!(current.context_files, vec!["file:src/investigate.rs"]);
}
