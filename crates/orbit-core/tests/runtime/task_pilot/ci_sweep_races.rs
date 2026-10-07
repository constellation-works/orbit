//! CI-sweep pilot races settle only when rejection or another active pilot
//! makes this run's assessment unnecessary.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use super::Workspace;

fn filed_task(workspace: &Workspace, title: &str) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "Repair the CI failure in the fixture.".into(),
            acceptance_criteria: vec!["The observed CI failure is repaired.".into()],
            plan: "Inspect README.md and repair the failing behavior.".into(),
            status: Some(TaskStatus::Proposed),
            tags: vec!["ci-failure-sweep".into(), "ci-failure:fixture-key".into()],
            context_files: vec!["file:README.md".into()],
            complexity: TaskComplexity::Low,
            ..Default::default()
        })
        .unwrap()
}

fn filing(task: &Task) -> Value {
    json!({
        "task_id": task.id,
        "failure_key": "fixture-key",
        "tested_commit": "0123abcd",
        "workflow": "ci",
        "job": "test",
        "step": "cargo test",
        "run_urls": ["https://github.com/example/repo/actions/runs/1"],
    })
}

fn assessment(task_id: &Value, snapshot: &Value) -> Value {
    json!({
        "task_id": task_id,
        "context_files_before": snapshot["context_files_before"],
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md contains the failing behavior.",
        "validation_approach": "Inspect the applied pilot assessment.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    })
}

fn assert_child_and_sweep_succeed(workspace: &Workspace, output: &Value) {
    assert_eq!(
        workspace.action("pipeline_success_guard", json!({"result": output}))["succeeded"],
        true,
        "child guard: {output}"
    );
    assert_eq!(
        workspace.action(
            "pipeline_success_guard",
            json!({
                "context": "CI-failure sweep pilot child",
                "results": [{"run_id": "pilot-child", "status": output["status"]}],
            }),
        )["succeeded"],
        true,
        "sweep guard: {output}"
    );
}

#[test]
fn rejected_or_archived_ci_sweep_task_settles_superseded_without_pilot_writes() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::ci_sweep_races::rejected_or_archived_ci_sweep_task_settles_superseded_without_pilot_writes",
    ) {
        return;
    }
    for (title, status) in [
        ("operator rejected", TaskStatus::Rejected),
        ("operator archived", TaskStatus::Archived),
    ] {
        let workspace = Workspace::new();
        let task = filed_task(&workspace, title);
        let prepared = workspace.prepare(&[&task.id]);
        if status == TaskStatus::Archived {
            workspace.runtime.archive_task(&task.id).unwrap();
        } else {
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        status: Some(status),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
        }
        let retired = workspace.runtime.get_task(&task.id).unwrap();
        let result = assessment(&json!(task.id), &prepared["tasks"][0]);
        let output = workspace.action(
            "apply_task_pilot_results",
            json!({
                "workspace_path": workspace.repo,
                "prepared": prepared,
                "results": [{
                    "partition_index": 0, "task_ids": [task.id], "tasks": [result],
                }],
                "ci_sweep_filing": filing(&task),
                "promotion_authorized": true,
            }),
        );

        assert_eq!(output["status"], "succeeded", "{output}");
        assert_eq!(output["outcome"], "superseded", "{output}");
        assert_eq!(output["superseded_count"], 1, "{output}");
        assert_eq!(output["applied_count"], 0, "{output}");
        assert_eq!(output["task_outcomes"][0]["reason"], "operator_rejected");
        assert_eq!(output["task_outcomes"][0]["status"], json!(status));
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), retired);
        assert!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .all(|entry| entry.event != "task_pilot_applied")
        );
        assert_child_and_sweep_succeed(&workspace, &output);
    }
}

#[test]
fn ci_sweep_excluded_by_an_active_pilot_settles_with_holder_and_holder_applies() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::ci_sweep_races::ci_sweep_excluded_by_an_active_pilot_settles_with_holder_and_holder_applies",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = filed_task(&workspace, "another pilot holds the CI finding");
    let holder_prepared = workspace.prepare(&[&task.id]);
    let holder_run = workspace.hold(&[&task.id]);

    let sweep_prepared = workspace.prepare(&[&task.id]);
    assert_eq!(sweep_prepared["task_count"], 0);
    assert_eq!(
        sweep_prepared["excluded"][0]["prepared_by_run_ids"],
        json!([holder_run])
    );
    let sweep_child = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": sweep_prepared,
            "results": [],
            "ci_sweep_filing": filing(&task),
            "promotion_authorized": true,
        }),
    );
    assert_eq!(sweep_child["status"], "succeeded", "{sweep_child}");
    assert_eq!(sweep_child["outcome"], "superseded", "{sweep_child}");
    assert_eq!(sweep_child["superseded_count"], 1, "{sweep_child}");
    assert_eq!(
        sweep_child["task_outcomes"][0]["reason"], "piloted_elsewhere",
        "{sweep_child}"
    );
    assert_eq!(sweep_child["task_outcomes"][0]["run_id"], holder_run);
    assert_child_and_sweep_succeed(&workspace, &sweep_child);

    let holder_result = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": holder_prepared,
            "results": [{
                "partition_index": 0,
                "task_ids": [task.id],
                "tasks": [assessment(&json!(task.id), &holder_prepared["tasks"][0])],
            }],
        }),
    );
    assert_eq!(holder_result["status"], "succeeded", "{holder_result}");
    assert_eq!(holder_result["applied_count"], 1, "{holder_result}");
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().context_files,
        ["file:README.md"]
    );
}

#[test]
fn other_status_changes_and_missing_ci_sweep_tasks_still_fail() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::ci_sweep_races::other_status_changes_and_missing_ci_sweep_tasks_still_fail",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let task = filed_task(&workspace, "status change outside the benign race");
    let prepared = workspace.prepare(&[&task.id]);
    workspace
        .runtime
        .update_task_as_human(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    let status_changed = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo,
            "prepared": prepared.clone(),
            "results": [{
                "partition_index": 0,
                "task_ids": [task.id],
                "tasks": [assessment(&json!(task.id), &prepared["tasks"][0])],
            }],
            "ci_sweep_filing": filing(&task),
            "promotion_authorized": true,
        }),
    );
    assert_eq!(status_changed["status"], "failed", "{status_changed}");
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Backlog
    );

    let missing_workspace = Workspace::new();
    let missing_task = filed_task(&missing_workspace, "deleted CI finding");
    let missing_prepared = missing_workspace.prepare(&[&missing_task.id]);
    missing_workspace
        .runtime
        .delete_task(&missing_task.id)
        .unwrap();
    let missing = missing_workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": missing_workspace.repo,
            "prepared": missing_prepared.clone(),
            "results": [{
                "partition_index": 0,
                "task_ids": [missing_task.id],
                "tasks": [assessment(&json!(missing_task.id), &missing_prepared["tasks"][0])],
            }],
            "ci_sweep_filing": filing(&missing_task),
            "promotion_authorized": true,
        }),
    );
    assert_eq!(missing["status"], "failed", "{missing}");
    assert_eq!(missing["task_outcomes"][0]["reason"], "task_deleted");
}
