use crate::task::{
    DependencyDeadEnd, Task, TaskHistoryEntry, TaskStatus, resolve_task_dependencies,
    satisfy_completed_archived_dependencies, unmet_task_dependencies,
    unsatisfiable_task_dependencies,
};
use chrono::Utc;
use std::collections::BTreeMap;

/// `Task::dependencies()` reads `blocked_by` *relations*, not the legacy
/// `dependencies` field, so the fixture must declare them that way.
fn task_with_dependencies(id: &str, dependencies: &[&str]) -> Task {
    let relations_yaml = dependencies
        .iter()
        .map(|dependency| format!("  - type: blocked_by\n    target: {dependency}\n"))
        .collect::<String>();
    let mut task = serde_yaml::from_str::<Task>(&format!(
        r#"id: {id}
title: Dependent task
description: Fixture.
acceptance_criteria: []
dependencies: []
plan: ""
execution_summary: ""
context_files: []
status: backlog
priority: medium
task_type: chore
created_at: 2026-01-01T00:00:00Z
updated_at: 2026-01-01T00:00:00Z
relations:
{relations_yaml}"#
    ))
    .expect("fixture task deserializes");
    assert_eq!(task.dependencies().len(), dependencies.len());
    task.relations.sort_by(|a, b| a.target.cmp(&b.target));
    task
}

fn status_index(entries: &[(&str, TaskStatus)]) -> BTreeMap<String, TaskStatus> {
    entries
        .iter()
        .map(|(id, status)| ((*id).to_string(), *status))
        .collect()
}

/// A history whose status transitions visit `statuses` in order, interleaved
/// with a non-status event so only transitions drive the rule.
fn history(statuses: &[TaskStatus]) -> Vec<TaskHistoryEntry> {
    let mut previous = None;
    let mut entries = Vec::new();
    for status in statuses {
        entries.push(TaskHistoryEntry {
            at: Utc::now(),
            by: "fixture".to_string(),
            event: "status_changed".to_string(),
            note: None,
            from_status: previous,
            to_status: Some(*status),
        });
        entries.push(TaskHistoryEntry {
            at: Utc::now(),
            by: "fixture".to_string(),
            event: "comment_added".to_string(),
            note: None,
            from_status: None,
            to_status: None,
        });
        previous = Some(*status);
    }
    entries
}

#[test]
fn completed_archived_dependency_is_satisfied_and_abandoned_one_stays_a_dead_end() {
    use TaskStatus::*;
    let task = task_with_dependencies("ORB-9", &["ORB-1", "ORB-2", "ORB-3", "ORB-4", "ORB-404"]);
    let mut statuses = status_index(&[
        ("ORB-1", Archived),
        ("ORB-2", Archived),
        ("ORB-3", Rejected),
        ("ORB-4", Archived),
    ]);
    let histories = BTreeMap::from([
        ("ORB-1", history(&[Backlog, Done, Archived])),
        ("ORB-2", history(&[Backlog, Review, Archived])),
        ("ORB-3", history(&[Backlog, Done, Rejected])),
    ]);
    let mut looked_up = Vec::new();

    satisfy_completed_archived_dependencies::<()>(&mut statuses, task.dependencies(), |id| {
        looked_up.push(id.to_string());
        Ok(histories.get(id).cloned())
    })
    .unwrap();

    assert_eq!(
        looked_up,
        vec!["ORB-1", "ORB-2", "ORB-4"],
        "only archived targets are looked up"
    );
    assert_eq!(statuses.get("ORB-1"), Some(&Done));
    assert_eq!(
        resolve_task_dependencies(&task, &statuses)[0].label(),
        "ORB-1 [done]"
    );
    let dead_ends = unsatisfiable_task_dependencies(&task, &statuses)
        .into_iter()
        .map(|dependency| (dependency.dependency_id, dependency.reason))
        .collect::<Vec<_>>();
    assert_eq!(
        dead_ends,
        vec![
            ("ORB-2".to_string(), DependencyDeadEnd::Archived),
            ("ORB-3".to_string(), DependencyDeadEnd::Rejected),
            // No readable history keeps the archived dead end.
            ("ORB-4".to_string(), DependencyDeadEnd::Archived),
            ("ORB-404".to_string(), DependencyDeadEnd::Missing),
        ]
    );
    assert_eq!(
        unmet_task_dependencies(&task, &statuses)
            .into_iter()
            .map(|dependency| dependency.id)
            .collect::<Vec<_>>(),
        vec!["ORB-2", "ORB-3", "ORB-4", "ORB-404"]
    );
}
