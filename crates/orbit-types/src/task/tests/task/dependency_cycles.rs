use crate::task::model::walk_dependencies_to_self;
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

fn layered_id(layer: usize, slot: usize) -> String {
    format!("ORB-{}", 10 + layer * 2 + slot)
}

#[test]
fn layered_diamond_work_is_bounded_by_reachable_edges() {
    // Every node in a layer depends on both nodes of the next layer, so the
    // number of distinct paths doubles per layer while nodes and edges grow
    // linearly.
    const LAYERS: usize = 40;
    let next_layer = |layer: usize| -> Vec<String> {
        if layer + 1 < LAYERS {
            vec![layered_id(layer + 1, 0), layered_id(layer + 1, 1)]
        } else {
            Vec::new()
        }
    };
    let mut edges = BTreeMap::new();
    for layer in 0..LAYERS {
        for slot in 0..2 {
            edges.insert(layered_id(layer, slot), next_layer(layer));
        }
    }

    let lookups = RefCell::new(BTreeMap::<String, usize>::new());
    let walk = walk_dependencies_to_self("ORB-1", &[layered_id(0, 0), layered_id(0, 1)], |id| {
        *lookups.borrow_mut().entry(id.to_string()).or_default() += 1;
        Ok::<_, TaskError>(edges.get(id).cloned())
    })
    .expect("diamond is acyclic");

    assert!(walk.cycle.is_none());
    let reachable_edges = 2 + 4 * (LAYERS - 1);
    assert_eq!(
        walk.edges_examined, reachable_edges,
        "each reachable edge must be examined once, not once per path"
    );
    let lookups = lookups.into_inner();
    assert_eq!(lookups.len(), 2 * LAYERS);
    assert!(
        lookups.values().all(|count| *count == 1),
        "each reachable adjacency must be fetched once: {lookups:?}"
    );
}

fn chain_lookup(
    len: usize,
    closes_cycle: bool,
) -> impl FnMut(&str) -> Result<Option<Vec<String>>, TaskError> {
    move |id| {
        let number: usize = id
            .strip_prefix("ORB-")
            .and_then(|number| number.parse().ok())
            .expect("chain ids are numeric");
        Ok(Some(if number < len {
            vec![format!("ORB-{}", number + 1)]
        } else if closes_cycle {
            vec!["ORB-0".to_string()]
        } else {
            Vec::new()
        }))
    }
}

const LONG_CHAIN: usize = 200_000;

#[test]
fn long_acyclic_chain_validates_without_recursion() {
    validate_task_dependencies_with(
        Some("ORB-0"),
        &["ORB-1".to_string()],
        chain_lookup(LONG_CHAIN, false),
    )
    .expect("long chain is acyclic");
}

#[test]
fn cycle_at_end_of_long_chain_reports_full_witness() {
    let error = validate_task_dependencies_with(
        Some("ORB-0"),
        &["ORB-1".to_string()],
        chain_lookup(LONG_CHAIN, true),
    )
    .expect_err("last task closes the cycle");
    let message = error.to_string();
    assert!(
        message.contains("task dependency cycle detected: ORB-0 -> ORB-1 -> ORB-2 -> "),
        "{}",
        &message[..message.len().min(200)]
    );
    assert!(
        message.ends_with(&format!(
            "ORB-{} -> ORB-{LONG_CHAIN} -> ORB-0",
            LONG_CHAIN - 1
        )),
        "{}",
        &message[message.len().saturating_sub(200)..]
    );
}

#[test]
fn cycles_not_through_updated_task_are_ignored() {
    let tasks = vec![
        task_with_dependencies("ORB-2", &["ORB-3"]),
        task_with_dependencies("ORB-3", &["ORB-2", "ORB-4"]),
        task_with_dependencies("ORB-4", &["ORB-4"]),
    ];
    validate_task_dependencies(&tasks, Some("ORB-1"), &["ORB-2".to_string()])
        .expect("pre-existing cycles elsewhere do not block this edit");
}

#[test]
fn lookup_errors_propagate_and_stop_the_walk() {
    let looked_up = RefCell::new(Vec::new());
    let error = validate_task_dependencies_with(
        Some("ORB-1"),
        &["ORB-2".to_string(), "ORB-3".to_string()],
        |id| {
            looked_up.borrow_mut().push(id.to_string());
            match id {
                "ORB-2" => Ok(Some(vec!["ORB-5".to_string()])),
                "ORB-5" => Err(TaskError::Invalid("store unavailable".to_string())),
                _ => Ok(Some(vec!["ORB-1".to_string()])),
            }
        },
    )
    .expect_err("lookup failure propagates");
    assert!(error.to_string().contains("store unavailable"), "{error}");
    assert_eq!(looked_up.into_inner(), vec!["ORB-2", "ORB-5"]);
}
