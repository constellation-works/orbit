//! Relation validation across workspace boundaries.

use std::fs;

use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};
use tempfile::TempDir;

use super::super::{RegisterWorkspaceParams, TaskRegistryStore};
use super::{envelope, store};

/// Relation validation reads a reachable subgraph rather than the whole
/// relation table, and the walk has to cross workspace boundaries: a relation
/// may target a task in another workspace, and a cycle routed through one is
/// still a cycle. Scoping the query to the writing workspace would pass this
/// write.
#[test]
fn relation_cycle_through_another_workspace_is_rejected() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let first = store
        .register_workspace(RegisterWorkspaceParams {
            partition_id: "logical-first-aaaaaa".into(),
            slug: "Logical First".into(),
            repo_fingerprint: None,
        })
        .expect("register first workspace");
    let second = store
        .register_workspace(RegisterWorkspaceParams {
            partition_id: "logical-second-bbbbbb".into(),
            slug: "Logical Second".into(),
            repo_fingerprint: None,
        })
        .expect("register second workspace");

    // `head` and `tail` live in the first workspace, `bridge` in the second,
    // so the only path from `tail` back to `head` leaves and re-enters.
    let head = register_indexed_task(&store, &first.partition_id, Vec::new());
    let bridge = register_indexed_task(&store, &second.partition_id, Vec::new());
    let tail = register_indexed_task(&store, &first.partition_id, Vec::new());

    store
        .replace_task_index(
            &second.partition_id,
            &envelope(
                &bridge,
                TaskStatus::Backlog,
                Vec::new(),
                vec![blocked_by(&tail)],
            ),
        )
        .expect("index bridge -> tail");
    store
        .replace_task_index(
            &first.partition_id,
            &envelope(
                &head,
                TaskStatus::Backlog,
                Vec::new(),
                vec![blocked_by(&bridge)],
            ),
        )
        .expect("index head -> bridge");

    let error = store
        .replace_task_index(
            &first.partition_id,
            &envelope(
                &tail,
                TaskStatus::Backlog,
                Vec::new(),
                vec![blocked_by(&head)],
            ),
        )
        .expect_err("cycle closing through the second workspace");
    assert!(
        error.to_string().contains("cycle"),
        "expected a cycle rejection, got: {error}"
    );
    assert!(
        store
            .indexed_relation_targets(&first.partition_id, &tail, TaskRelationType::BlockedBy)
            .expect("relations after rejected cycle")
            .is_empty(),
        "a rejected cycle must not write relation rows"
    );
}

fn blocked_by(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::BlockedBy,
        target: target.to_string(),
    }
}

/// Allocate, register, and index one task, returning its id.
fn register_indexed_task(
    store: &TaskRegistryStore,
    partition_id: &str,
    relations: Vec<TaskRelation>,
) -> String {
    let task_id = store
        .allocate_task_id(partition_id)
        .expect("allocate task id");
    let path = store
        .canonical_task_bundle_path(partition_id, &task_id)
        .expect("canonical bundle path");
    fs::create_dir_all(&path).expect("create bundle");
    store
        .register_task_bundle(&task_id, partition_id, &path)
        .expect("register bundle");
    store
        .replace_task_index(
            partition_id,
            &envelope(&task_id, TaskStatus::Backlog, Vec::new(), relations),
        )
        .expect("index task");
    task_id
}
