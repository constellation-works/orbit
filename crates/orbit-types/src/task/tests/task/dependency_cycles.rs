use crate::task::{Task, TaskError, validate_task_dependencies, validate_task_dependencies_with};
use std::cell::RefCell;
use std::collections::BTreeMap;

fn task_with_dependencies(id: &str, dependencies: &[&str]) -> Task {
    let relations_block = if dependencies.is_empty() {
        "relations: []".to_string()
    } else {
        let relations_yaml = dependencies
            .iter()
            .map(|dependency| format!("  - type: blocked_by\n    target: {dependency}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!("relations:\n{relations_yaml}")
    };
    let task = serde_yaml::from_str::<Task>(&format!(
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
{relations_block}"#
    ))
    .expect("fixture task deserializes");
    assert_eq!(task.dependencies().len(), dependencies.len());
    task
}

#[test]
fn rejects_self_dependency() {
    let error = validate_task_dependencies(&[], Some("ORB-1"), &["ORB-1".to_string()])
        .expect_err("self-dependency");
    assert!(matches!(error, TaskError::Invalid(_)));
    assert!(
        error
            .to_string()
            .contains("cannot declare a self-dependency"),
        "{error}"
    );
}

#[test]
fn rejects_two_node_cycle() {
    let tasks = vec![
        task_with_dependencies("ORB-1", &["ORB-2"]),
        task_with_dependencies("ORB-2", &["ORB-1"]),
    ];
    let error = validate_task_dependencies(&tasks, Some("ORB-1"), &["ORB-2".to_string()])
        .expect_err("two-node cycle");
    assert!(error.to_string().contains("task dependency cycle detected"));
    assert!(
        error.to_string().contains("ORB-1 -> ORB-2 -> ORB-1"),
        "{error}"
    );
}

#[test]
fn rejects_multi_hop_cycle() {
    let tasks = vec![
        task_with_dependencies("ORB-1", &["ORB-2"]),
        task_with_dependencies("ORB-2", &["ORB-3"]),
        task_with_dependencies("ORB-3", &["ORB-1"]),
    ];
    let error = validate_task_dependencies(&tasks, Some("ORB-1"), &["ORB-2".to_string()])
        .expect_err("multi-hop cycle");
    assert!(
        error
            .to_string()
            .contains("ORB-1 -> ORB-2 -> ORB-3 -> ORB-1"),
        "{error}"
    );
}

#[test]
fn allows_acyclic_chain() {
    let tasks = vec![
        task_with_dependencies("ORB-1", &[]),
        task_with_dependencies("ORB-2", &["ORB-3"]),
        task_with_dependencies("ORB-3", &[]),
    ];
    validate_task_dependencies(&tasks, Some("ORB-1"), &["ORB-2".to_string()])
        .expect("acyclic chain");
}

#[test]
fn lookup_visits_only_reachable_tasks_and_caches_repeats() {
    let mut edges = BTreeMap::new();
    edges.insert("ORB-2", vec!["ORB-4".to_string()]);
    edges.insert("ORB-3", vec!["ORB-4".to_string()]);
    edges.insert("ORB-4", Vec::new());
    edges.insert("ORB-99", vec!["ORB-1".to_string()]);

    let looked_up = RefCell::new(Vec::new());
    validate_task_dependencies_with(
        Some("ORB-1"),
        &["ORB-2".to_string(), "ORB-3".to_string()],
        |id| {
            looked_up.borrow_mut().push(id.to_string());
            Ok::<_, TaskError>(edges.get(id).cloned())
        },
    )
    .expect("diamond is acyclic");

    let looked_up = looked_up.into_inner();
    assert_eq!(
        looked_up,
        vec![
            "ORB-2".to_string(),
            "ORB-4".to_string(),
            "ORB-3".to_string()
        ]
    );
    assert!(
        !looked_up.iter().any(|id| id == "ORB-1" || id == "ORB-99"),
        "must not look up the updated task or unreachable peers: {looked_up:?}"
    );
}

#[test]
fn missing_targets_are_leaves() {
    validate_task_dependencies(
        &[task_with_dependencies("ORB-2", &[])],
        Some("ORB-1"),
        &["ORB-404".to_string()],
    )
    .expect("unknown dependency is not a cycle");
}
