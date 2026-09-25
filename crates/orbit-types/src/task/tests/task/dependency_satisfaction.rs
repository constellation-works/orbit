use crate::task::{
    DependencyDeadEnd, TASK_REFERENCE_NOT_VERIFIABLE_HERE, Task, TaskReferenceIndex, TaskStatus,
    resolve_task_dependencies, resolve_task_dependencies_with_index,
    resolve_task_relations_with_index, task_dependencies_ready, task_dependencies_ready_with_index,
    unmet_task_dependencies, unmet_task_dependencies_with_index, unsatisfiable_task_dependencies,
    unsatisfiable_task_dependencies_with_index,
};
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

#[test]
fn only_archived_and_rejected_are_dead_ends() {
    assert_eq!(
        TaskStatus::Archived.dependency_dead_end(),
        Some(DependencyDeadEnd::Archived)
    );
    assert_eq!(
        TaskStatus::Rejected.dependency_dead_end(),
        Some(DependencyDeadEnd::Rejected)
    );
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Done,
        TaskStatus::Blocked,
        TaskStatus::Someday,
    ] {
        assert_eq!(
            status.dependency_dead_end(),
            None,
            "{status} must remain a legitimate wait"
        );
    }
}

#[test]
fn done_only_still_satisfies_a_dependency() {
    // Guardrail: this task made dead ends fail loudly; it must not have
    // widened what counts as satisfied.
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
        TaskStatus::Blocked,
        TaskStatus::Archived,
        TaskStatus::Rejected,
        TaskStatus::Someday,
    ] {
        assert!(
            !status.satisfies_dependency(),
            "{status} must not satisfy a dependency"
        );
    }
    assert!(TaskStatus::Done.satisfies_dependency());
}

#[test]
fn archived_dependency_is_unsatisfiable() {
    let task = task_with_dependencies("ORB-2", &["ORB-1"]);
    let statuses = status_index(&[("ORB-1", TaskStatus::Archived)]);

    let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

    assert_eq!(unsatisfiable.len(), 1);
    assert_eq!(unsatisfiable[0].task_id, "ORB-2");
    assert_eq!(unsatisfiable[0].dependency_id, "ORB-1");
    assert_eq!(unsatisfiable[0].status, "archived");
    assert_eq!(unsatisfiable[0].reason, DependencyDeadEnd::Archived);
    assert!(unsatisfiable[0].label().contains("ORB-2 blocked_by ORB-1"));
}

#[test]
fn rejected_dependency_is_unsatisfiable() {
    let task = task_with_dependencies("ORB-2", &["ORB-1"]);
    let statuses = status_index(&[("ORB-1", TaskStatus::Rejected)]);

    let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

    assert_eq!(unsatisfiable.len(), 1);
    assert_eq!(unsatisfiable[0].reason, DependencyDeadEnd::Rejected);
}

#[test]
fn dangling_dependency_is_unsatisfiable() {
    let task = task_with_dependencies("ORB-2", &["ORB-404"]);
    let statuses = status_index(&[]);

    let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

    assert_eq!(unsatisfiable.len(), 1);
    assert_eq!(unsatisfiable[0].dependency_id, "ORB-404");
    assert_eq!(unsatisfiable[0].status, "missing");
    assert_eq!(unsatisfiable[0].reason, DependencyDeadEnd::Missing);
}

#[test]
fn foreign_dependency_is_marked_ready_and_not_a_dead_end() {
    let task = task_with_dependencies("ORB-2", &["DK-404"]);
    let statuses = status_index(&[("ORB-2", TaskStatus::Backlog)]);

    assert_eq!(
        resolve_task_dependencies(&task, &statuses)[0].status,
        TASK_REFERENCE_NOT_VERIFIABLE_HERE
    );
    assert!(task_dependencies_ready(&task, &statuses));
    assert!(unmet_task_dependencies(&task, &statuses).is_empty());
    assert!(unsatisfiable_task_dependencies(&task, &statuses).is_empty());
}

#[test]
fn shared_reference_index_preserves_reference_classification_across_helpers() {
    let task = task_with_dependencies(
        "ORB-9000",
        &[
            "ORB-1",
            "ORB-404",
            "LEGACY-404",
            "OTHER-404",
            "not-a-task-id",
        ],
    );
    let statuses = status_index(&[
        ("ORB-1", TaskStatus::Done),
        ("LEGACY-1", TaskStatus::Backlog),
    ]);
    let reference_index = TaskReferenceIndex::from_status_index(&statuses);

    assert_eq!(reference_index.indexed_task_count(), statuses.len());
    assert_eq!(
        resolve_task_dependencies_with_index(&task, &statuses, &reference_index)
            .into_iter()
            .map(|dependency| dependency.status)
            .collect::<Vec<_>>(),
        vec![
            "missing".to_string(),
            "done".to_string(),
            "missing".to_string(),
            TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string(),
            "missing".to_string(),
        ]
    );
    assert_eq!(
        resolve_task_relations_with_index(&task, &statuses, &reference_index)
            .into_iter()
            .filter_map(|relation| relation.verification)
            .collect::<Vec<_>>(),
        vec![TASK_REFERENCE_NOT_VERIFIABLE_HERE.to_string()]
    );
    assert!(!task_dependencies_ready_with_index(
        &task,
        &statuses,
        &reference_index
    ));
    assert_eq!(
        unmet_task_dependencies_with_index(&task, &statuses, &reference_index).len(),
        3
    );
    assert_eq!(
        unsatisfiable_task_dependencies_with_index(&task, &statuses, &reference_index).len(),
        3
    );
}

#[test]
fn shared_reference_index_scans_a_snapshot_once_for_large_mixed_batches() {
    const INDEXED_TASKS: usize = 1_000;
    const UNRESOLVED_REFERENCES: usize = 2_000;

    let statuses = (0..INDEXED_TASKS)
        .map(|number| (format!("ORB-{number}"), TaskStatus::Done))
        .collect::<BTreeMap<_, _>>();
    let dependencies = (0..UNRESOLVED_REFERENCES)
        .map(|number| {
            if number % 10 == 0 {
                format!("ORB-{}", INDEXED_TASKS + number)
            } else {
                format!("EXT-{number}")
            }
        })
        .collect::<Vec<_>>();
    let dependency_refs = dependencies.iter().map(String::as_str).collect::<Vec<_>>();
    let task = task_with_dependencies("ORB-9000", &dependency_refs);

    let reference_index = TaskReferenceIndex::from_status_index(&statuses);
    let resolved = resolve_task_dependencies_with_index(&task, &statuses, &reference_index);

    // The old path rebuilt this set for every unresolved valid reference:
    // 2,000,000 key examinations here. This snapshot indexes 1,000 keys
    // once, independent of the 2,000 relation checks below.
    assert_eq!(reference_index.indexed_task_count(), INDEXED_TASKS);
    assert_eq!(resolved.len(), UNRESOLVED_REFERENCES);
    assert_eq!(
        resolved
            .iter()
            .filter(|dependency| dependency.status == "missing")
            .count(),
        UNRESOLVED_REFERENCES / 10
    );
    assert_eq!(
        resolved
            .iter()
            .filter(|dependency| dependency.status == TASK_REFERENCE_NOT_VERIFIABLE_HERE)
            .count(),
        UNRESOLVED_REFERENCES - (UNRESOLVED_REFERENCES / 10)
    );
}

#[test]
fn shared_reference_index_has_bounded_work_for_mostly_resolved_batches() {
    const INDEXED_TASKS: usize = 1_000;
    const RESOLVED_REFERENCES: usize = 900;
    const LOCAL_MISSING_REFERENCES: usize = 50;
    const FOREIGN_REFERENCES: usize = 50;

    let statuses = (0..INDEXED_TASKS)
        .map(|number| (format!("ORB-{number}"), TaskStatus::Done))
        .collect::<BTreeMap<_, _>>();
    let dependencies = (0..RESOLVED_REFERENCES)
        .map(|number| format!("ORB-{number}"))
        .chain(
            (0..LOCAL_MISSING_REFERENCES).map(|number| format!("ORB-{}", INDEXED_TASKS + number)),
        )
        .chain((0..FOREIGN_REFERENCES).map(|number| format!("EXT-{number}")))
        .collect::<Vec<_>>();
    let dependency_refs = dependencies.iter().map(String::as_str).collect::<Vec<_>>();
    let task = task_with_dependencies("ORB-9000", &dependency_refs);

    let reference_index = TaskReferenceIndex::from_status_index(&statuses);
    let resolved = resolve_task_dependencies_with_index(&task, &statuses, &reference_index);

    // Before: the 100 unresolved valid references would examine 100,000
    // keys. After: this bounded snapshot examines 1,000 keys once; the
    // 900 known targets retain their status-map fast path.
    assert_eq!(reference_index.indexed_task_count(), INDEXED_TASKS);
    assert_eq!(
        resolved
            .iter()
            .filter(|dependency| dependency.status == "done")
            .count(),
        RESOLVED_REFERENCES
    );
    assert_eq!(
        resolved
            .iter()
            .filter(|dependency| dependency.status == "missing")
            .count(),
        LOCAL_MISSING_REFERENCES
    );
    assert_eq!(
        resolved
            .iter()
            .filter(|dependency| dependency.status == TASK_REFERENCE_NOT_VERIFIABLE_HERE)
            .count(),
        FOREIGN_REFERENCES
    );
}

#[test]
fn in_flight_dependency_is_unmet_but_not_unsatisfiable() {
    let task = task_with_dependencies("ORB-2", &["ORB-1"]);
    for status in [
        TaskStatus::Backlog,
        TaskStatus::Proposed,
        TaskStatus::InProgress,
        TaskStatus::Review,
    ] {
        let statuses = status_index(&[("ORB-1", status)]);

        assert!(
            unsatisfiable_task_dependencies(&task, &statuses).is_empty(),
            "{status} must not fail dispatch fast"
        );
        assert_eq!(
            unmet_task_dependencies(&task, &statuses).len(),
            1,
            "{status} must still be reported as an unmet wait"
        );
    }
}

#[test]
fn satisfied_dependency_is_neither_unmet_nor_unsatisfiable() {
    let task = task_with_dependencies("ORB-2", &["ORB-1"]);
    let statuses = status_index(&[("ORB-1", TaskStatus::Done)]);

    assert!(unsatisfiable_task_dependencies(&task, &statuses).is_empty());
    assert!(unmet_task_dependencies(&task, &statuses).is_empty());
}

#[test]
fn mixed_edges_report_only_the_dead_ends_as_unsatisfiable() {
    let task = task_with_dependencies("ORB-4", &["ORB-1", "ORB-2", "ORB-3"]);
    let statuses = status_index(&[
        ("ORB-1", TaskStatus::Backlog),
        ("ORB-2", TaskStatus::Archived),
        ("ORB-3", TaskStatus::Done),
    ]);

    let unsatisfiable = unsatisfiable_task_dependencies(&task, &statuses);

    assert_eq!(unsatisfiable.len(), 1);
    assert_eq!(unsatisfiable[0].dependency_id, "ORB-2");
    // The plain unmet rollup is unchanged: it still reports both.
    let unmet: Vec<String> = unmet_task_dependencies(&task, &statuses)
        .into_iter()
        .map(|dependency| dependency.id)
        .collect();
    assert_eq!(unmet, vec!["ORB-1".to_string(), "ORB-2".to_string()]);
}
