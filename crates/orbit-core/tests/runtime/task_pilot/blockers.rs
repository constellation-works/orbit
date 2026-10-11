//! Prerequisite prose becomes durable dependencies at the real pilot boundary.

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use super::{Workspace, races::apply_input};
use crate::dispatch_admission::isolated;

fn target(workspace: &Workspace, status: TaskStatus) -> Task {
    workspace
        .runtime
        .add_task(TaskAddParams {
            title: format!("Prerequisite in {status}"),
            description: "A local prerequisite.".into(),
            plan: "Inspect README.md.".into(),
            acceptance_criteria: vec!["The prerequisite is complete.".into()],
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
}

fn input(workspace: &Workspace, task: &Task, warnings: &[String]) -> Value {
    let prepared = workspace.prepare(&[&task.id]);
    let mut input = apply_input(workspace, &prepared);
    input["results"][0]["tasks"][0]["blocked_by"] = json!(warnings);
    input
}

fn apply(workspace: &Workspace, task: &Task, warnings: &[String]) -> Value {
    let result = workspace.action("apply_task_pilot_results", input(workspace, task, warnings));
    assert_eq!(result["status"], "succeeded", "{result}");
    assert_eq!(result["applied_count"], 1, "{result}");
    result
}

fn readiness(workspace: &Workspace, task: &Task) -> Value {
    let result = workspace
        .runtime
        .workspace_auto_readiness(&[], None, 50, &[])
        .unwrap();
    result["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["task_id"] == task.id)
        .unwrap_or_else(|| panic!("task missing from readiness: {result}"))
        .clone()
}

#[test]
fn named_unfinished_blockers_survive_approval_and_repilot_until_done() {
    if !isolated(
        "task_pilot::blockers::named_unfinished_blockers_survive_approval_and_repilot_until_done",
    ) {
        return;
    }
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Blocked,
        TaskStatus::Review,
    ] {
        let workspace = Workspace::new();
        let blocker = target(&workspace, status);
        let existing = target(&workspace, TaskStatus::Done);
        let task = workspace.task("Repair after the prerequisite");
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    dependencies: Some(vec![existing.id.clone()]),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        let warnings = vec![format!(
            "Wait for `{}` to land, then use {}.",
            blocker.id, blocker.id
        )];
        let first_input = input(&workspace, &task, &warnings);
        let first = workspace.action("apply_task_pilot_results", first_input.clone());
        assert_eq!(first["status"], "succeeded", "{first}");
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().dependencies(),
            [existing.id.clone(), blocker.id.clone()]
        );
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        let replay = workspace.action("apply_task_pilot_results", first_input);
        assert_eq!(replay["status"], "succeeded", "{replay}");
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap(),
            history
        );
        apply(&workspace, &task, &warnings);
        assert_eq!(
            workspace.runtime.get_task(&task.id).unwrap().dependencies(),
            [existing.id, blocker.id.clone()]
        );
        workspace
            .runtime
            .approve_task(&task.id, None, None)
            .unwrap();
        let waiting = readiness(&workspace, &task);
        assert_eq!(waiting["eligible"], false, "{waiting}");
        assert_eq!(waiting["reason"], "unmet_dependency", "{waiting}");
        assert_eq!(
            workspace
                .runtime
                .task_pilot_blocker_warnings(&task.id)
                .unwrap(),
            warnings
        );
        if status == TaskStatus::Review {
            workspace
                .runtime
                .approve_task(&blocker.id, None, None)
                .unwrap();
            let ready = readiness(&workspace, &task);
            assert_eq!(ready["eligible"], true, "{ready}");
            assert_eq!(ready["reason"], "ready", "{ready}");
        }
    }
}

#[test]
fn terminal_missing_foreign_and_free_text_blockers_remain_warnings() {
    if !isolated(
        "task_pilot::blockers::terminal_missing_foreign_and_free_text_blockers_remain_warnings",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let done = target(&workspace, TaskStatus::Done);
    let rejected = target(&workspace, TaskStatus::Rejected);
    let archived = target(&workspace, TaskStatus::Proposed);
    workspace.runtime.archive_task(&archived.id).unwrap();
    let foreign = workspace
        .linked_workspace()
        .add_task(TaskAddParams {
            title: "Other workspace prerequisite".into(),
            ..Default::default()
        })
        .unwrap();
    for warning in [
        format!("Await {}", done.id),
        format!("Await {}", archived.id),
        format!("Await {}", rejected.id),
        format!("Await {}", foreign.id),
        "Await ORB-999999999".into(),
        "Await operator credentials".into(),
    ] {
        let task = workspace.task("Warning-only prerequisite");
        apply(&workspace, &task, std::slice::from_ref(&warning));
        let current = workspace.runtime.get_task(&task.id).unwrap();
        assert!(current.dependencies().is_empty(), "{current:?}");
        assert_eq!(current.status, TaskStatus::Proposed);
        assert_eq!(
            workspace
                .runtime
                .task_pilot_blocker_warnings(&task.id)
                .unwrap(),
            [warning]
        );
        apply(&workspace, &task, &[]);
        assert!(
            workspace
                .runtime
                .task_pilot_blocker_warnings(&task.id)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn self_blockers_are_dropped_with_a_warning_and_other_metadata_applies() {
    if !isolated(
        "task_pilot::blockers::self_blockers_are_dropped_with_a_warning_and_other_metadata_applies",
    ) {
        return;
    }
    let workspace = Workspace::new();
    let blocker = target(&workspace, TaskStatus::Backlog);
    let existing = target(&workspace, TaskStatus::Done);
    for case in 0..4 {
        let task = workspace.task(&format!("Self-reference case {case}"));
        workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    dependencies: Some(vec![existing.id.clone()]),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        let (warnings, expected) = match case {
            0 => (vec![task.id.clone()], vec![]),
            1 => (vec![format!("Wait for `{}` to land.", task.id)], vec![]),
            2 => {
                let other = format!("Wait for {} to land.", blocker.id);
                let free_text = "Await operator credentials".to_string();
                (
                    vec![task.id.clone(), other.clone(), free_text.clone()],
                    vec![other, free_text],
                )
            }
            _ => (
                vec![format!(
                    "Wait for `{}` and `{}`; {} is repeated. Keep {}0 as evidence.",
                    task.id, blocker.id, task.id, task.id
                )],
                vec![format!(
                    "Wait for `` and `{}`;  is repeated. Keep {}0 as evidence.",
                    blocker.id, task.id
                )],
            ),
        };
        let candidate = input(&workspace, &task, &warnings);
        let applied = workspace.action("apply_task_pilot_results", candidate.clone());
        assert_eq!(applied["status"], "succeeded", "{applied}");
        assert_eq!(applied["applied_count"], 1, "{applied}");
        assert_eq!(applied["repair_count"], 0, "{applied}");
        assert_eq!(applied["partition_decisions"][0]["outcome"], "applied");
        let recorded = &applied["tasks"][0];
        assert_eq!(recorded["blocked_by"], json!(expected), "{recorded}");
        let warning = &recorded["pilot_normalizations"][0];
        assert_eq!(warning["field"], "blocked_by", "{warning}");
        assert_eq!(warning["dropped_self_reference"], task.id, "{warning}");
        let note = warning["note"].as_str().unwrap();
        assert!(
            note.contains("warning") && note.contains(&task.id),
            "{note}"
        );
        let current = workspace.runtime.get_task(&task.id).unwrap();
        let mut dependencies = vec![existing.id.clone()];
        if case >= 2 {
            dependencies.push(blocker.id.clone());
        }
        assert_eq!(current.dependencies(), dependencies, "{current:?}");
        assert_eq!(current.context_files, ["file:README.md"]);
        assert_eq!(current.complexity, Some(TaskComplexity::Low));
        let history = workspace.runtime.get_task_history(&task.id).unwrap();
        assert!(
            history.iter().any(|event| {
                event.event == "task_pilot_applied"
                    && event
                        .note
                        .as_deref()
                        .is_some_and(|text| text.contains(note))
            }),
            "the atomic application history must retain its warning: {history:?}"
        );
        assert_eq!(
            workspace
                .runtime
                .task_pilot_blocker_warnings(&task.id)
                .unwrap(),
            expected
        );
        let replay = workspace.action("apply_task_pilot_results", candidate);
        assert_eq!(replay["status"], "succeeded", "{replay}");
        assert_eq!(replay["tasks"][0]["outcome"], "already_applied");
        assert_eq!(replay["tasks"][0]["pilot_normalizations"][0], *warning);
        assert_eq!(
            workspace.runtime.get_task_history(&task.id).unwrap(),
            history
        );
    }

    // Agent-supplied normalization notes cannot invent a host warning or
    // inject text into the atomic history of an otherwise clean assessment.
    let task = workspace.task("Clean assessment with an untrusted note");
    let mut candidate = input(&workspace, &task, &[]);
    candidate["results"][0]["tasks"][0]["pilot_normalizations"] = json!([{
        "field": "blocked_by", "dropped_self_reference": task.id,
        "note": "untrusted warning supplied by the pilot",
    }]);
    let applied = workspace.action("apply_task_pilot_results", candidate);
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert!(applied["tasks"][0].get("pilot_normalizations").is_none());
    let history = workspace.runtime.get_task_history(&task.id).unwrap();
    assert!(
        history.iter().all(|event| {
            event
                .note
                .as_deref()
                .is_none_or(|note| !note.contains("untrusted warning"))
        }),
        "pilot prose must not become a host-authored history warning: {history:?}"
    );
}

#[test]
fn a_pilot_dependency_cycle_refuses_the_entire_assessment() {
    if !isolated("task_pilot::blockers::a_pilot_dependency_cycle_refuses_the_entire_assessment") {
        return;
    }
    let workspace = Workspace::new();
    let task = workspace.task("Cyclic assessment");
    let blocker = target(&workspace, TaskStatus::Backlog);
    workspace
        .runtime
        .update_task_as_human(
            &blocker.id,
            TaskUpdateParams {
                dependencies: Some(vec![task.id.clone()]),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    for warnings in [
        vec![format!("Wait for {}", blocker.id)],
        vec![format!("Wait for {} and {}", task.id, blocker.id)],
    ] {
        let candidate = input(&workspace, &task, &warnings);
        let before = workspace.runtime.get_task(&task.id).unwrap();
        let comments = workspace.runtime.get_task_comments(&task.id).unwrap();
        let result = workspace.action("apply_task_pilot_results", candidate);
        assert_eq!(result["status"], "failed", "{result}");
        assert_eq!(result["applied_count"], 0, "{result}");
        assert_eq!(workspace.runtime.get_task(&task.id).unwrap(), before);
        assert_eq!(
            workspace.runtime.get_task_comments(&task.id).unwrap(),
            comments
        );
    }
}
