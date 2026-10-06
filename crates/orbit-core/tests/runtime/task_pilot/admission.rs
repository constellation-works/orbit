//! Applied findings hold directly filed backlog tasks across the real drain,
//! ship-selection and readiness boundaries, with assessment-scoped decisions.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use serde_json::json;

use super::Workspace;
use crate::dispatch_admission::isolated;

fn backlog_task(workspace: &Workspace) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: "Directly filed backlog work".into(),
            description: "Repair the README fixture.".into(),
            acceptance_criteria: vec!["The fixture repair is observable.".into()],
            plan: "Inspect README.md.".into(),
            status: Some(TaskStatus::Backlog),
            complexity: TaskComplexity::Low,
            context_files: vec!["file:README.md".into()],
            ..Default::default()
        })
        .unwrap()
}

/// Drive the same default, non-promoting prepare/apply actions that routine
/// task-pilot uses, rather than seeding an admission-only representation.
fn assess(workspace: &Workspace, task: &Task, finding: Option<&str>) {
    let prepared = workspace.prepare(&[&task.id]);
    let current = workspace.runtime.get_task(&task.id).unwrap();
    let mut assessment = json!({
        "task_id": task.id,
        "context_files_before": current.context_files,
        "context_files_after": ["file:README.md"],
        "disposition": "selectors", "recommended_crew": "fixture",
        "recommended_complexity": "low", "confidence": "high",
        "assessment_rationale": "README.md contains the affected material.",
        "validation_approach": "Exercise drain admission.",
        "evidence_gaps": [], "reassessment_triggers": [], "blocked_by": [],
        "adr_conflicts": [], "utility_warnings": [], "surface_warnings": [],
        "duplicate_of": null, "already_landed": null,
    });
    if let Some(field) = finding {
        assessment[field] = json!({
            "task_id": task.id,
            "evidence": "The same README repair is already covered by existing work.",
        });
    }
    let applied = workspace.action(
        "apply_task_pilot_results",
        json!({
            "workspace_path": workspace.repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": [task.id], "tasks": [assessment]}],
        }),
    );
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["applied_count"], 1, "{applied}");
    assert_eq!(applied["tasks"][0]["task_id"], task.id, "{applied}");
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().status,
        TaskStatus::Backlog
    );
}

fn assert_admission(workspace: &Workspace, task: &Task, reason: Option<&str>) {
    for input in [json!({}), json!({"task_ids": [task.id]})] {
        let output = workspace.action("list_backlog_tasks", input);
        if let Some(reason) = reason {
            assert_eq!(output["task_ids"], json!([]), "{output}");
            let exclusion = output["excluded"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == task.id)
                .unwrap();
            assert_eq!(exclusion["reason"], reason, "{output}");
            assert!(!exclusion["detail"].as_str().unwrap().is_empty());
        } else {
            assert_eq!(output["task_ids"], json!([task.id]), "{output}");
        }
    }
    let wave = workspace.action(
        "classify_workspace_auto_tasks",
        json!({"max_active_leaf_runs": 2}),
    );
    assert_eq!(
        wave["loose_task_ids"],
        if reason.is_some() {
            json!([])
        } else {
            json!([task.id])
        },
        "{wave}"
    );
    let readiness = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    let entry = readiness["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task.id)
        .unwrap_or_else(|| panic!("task missing from readiness: {readiness}"));
    assert_eq!(entry["eligible"], reason.is_none(), "{readiness}");
    if let Some(reason) = reason {
        assert_eq!(entry["reason"], reason, "{readiness}");
    }
}

#[test]
fn latest_pilot_finding_holds_backlog_until_a_clear_assessment() {
    if !isolated(
        "task_pilot::admission::latest_pilot_finding_holds_backlog_until_a_clear_assessment",
    ) {
        return;
    }
    for (field, reason) in [
        ("duplicate_of", "pilot_duplicate"),
        ("already_landed", "pilot_already_landed"),
    ] {
        let workspace = Workspace::new();
        let task = backlog_task(&workspace);
        assert_admission(&workspace, &task, None);
        assess(&workspace, &task, Some(field));
        assert_admission(&workspace, &task, Some(reason));
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    description: Some(
                        "Edited scope still needs a decision about the finding.".into(),
                    ),
                    comment: Some("The operator is reviewing the assessment.".into()),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assert_admission(&workspace, &task, Some(reason));
        assess(&workspace, &task, None);
        assert_admission(&workspace, &task, None);
    }
}

#[test]
fn human_pilot_decisions_release_only_the_current_assessment() {
    if !isolated("task_pilot::admission::human_pilot_decisions_release_only_the_current_assessment")
    {
        return;
    }
    for decision in ["approve-anyway", "clear"] {
        for (field, reason) in [
            ("duplicate_of", "pilot_duplicate"),
            ("already_landed", "pilot_already_landed"),
        ] {
            let workspace = Workspace::new();
            let task = backlog_task(&workspace);
            let comment = format!("task-pilot-admission: {decision}\nReviewed the finding.");
            // A decision made before an assessment cannot approve future findings.
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
            assess(&workspace, &task, Some(field));
            // Canonical agent provenance cannot forge the human release path.
            workspace
                .runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment.clone()),
                        ..Default::default()
                    },
                    Some("codex".into()),
                    Some("gpt-6.1-sol".into()),
                )
                .unwrap();
            assert_admission(&workspace, &task, Some(reason));
            workspace
                .runtime
                .update_task_as_human(
                    &task.id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        ..Default::default()
                    },
                    "human:fixture".into(),
                )
                .unwrap();
            assert_admission(&workspace, &task, None);
            assess(&workspace, &task, Some(field));
            assert_admission(&workspace, &task, Some(reason));
        }
    }
}
