//! One prerequisite, one edge: `dependencies` and `blocked_by` are the same input.

use super::v2::*;
use orbit_types::task::{TaskRelation, TaskRelationType, TaskStatus};

fn prerequisite(store: &TaskV2Store) -> String {
    store
        .create_task(create_params("Prerequisite", TaskStatus::Done))
        .expect("create prerequisite")
        .id
}

fn blocked_by(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::BlockedBy,
        target: target.to_string(),
        regression: None,
    }
}

fn related_to(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::RelatedTo,
        target: target.to_string(),
        regression: None,
    }
}

#[test]
fn same_prerequisite_in_dependencies_and_blocked_by_is_refused_with_correction() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let pre = prerequisite(&store);
    let mut params = create_params("Dependent", TaskStatus::Backlog);
    params.dependencies = vec![pre.clone()];
    params.relations = vec![blocked_by(&pre)];
    let error = store
        .create_task(params)
        .expect_err("the same prerequisite in both fields must stay refused");
    let message = error.to_string();
    assert!(
        message.contains(&format!("duplicate task relation BlockedBy -> '{pre}'")),
        "refusal names the duplicate edge, got: {message}"
    );
    assert!(
        message.contains("dependencies") && message.contains("blocked_by"),
        "refusal names both prerequisite representations, got: {message}"
    );
    assert!(
        message.contains("once"),
        "refusal tells the caller to supply the prerequisite once, got: {message}"
    );
}

#[test]
fn single_dependency_creates_one_blocking_edge() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let pre = prerequisite(&store);
    let mut params = create_params("Dependent", TaskStatus::Backlog);
    params.dependencies = vec![pre.clone()];
    let task = store.create_task(params).expect("create dependent");
    assert_eq!(task.dependencies(), vec![pre.clone()]);
    let blocked: Vec<_> = task
        .relations
        .iter()
        .filter(|relation| relation.relation_type == TaskRelationType::BlockedBy)
        .collect();
    assert_eq!(blocked.len(), 1, "one prerequisite is one edge");
    assert_eq!(blocked[0].target, pre);
}

#[test]
fn single_blocked_by_relation_creates_one_blocking_edge() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let pre = prerequisite(&store);
    let mut params = create_params("Dependent", TaskStatus::Backlog);
    params.relations = vec![blocked_by(&pre)];
    let task = store.create_task(params).expect("create dependent");
    assert_eq!(task.dependencies(), vec![pre.clone()]);
    let blocked: Vec<_> = task
        .relations
        .iter()
        .filter(|relation| relation.relation_type == TaskRelationType::BlockedBy)
        .collect();
    assert_eq!(blocked.len(), 1, "one prerequisite is one edge");
    assert_eq!(blocked[0].target, pre);
}

#[test]
fn unrelated_duplicate_relations_stay_refused() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let pre = prerequisite(&store);
    let mut params = create_params("Dependent", TaskStatus::Backlog);
    params.relations = vec![related_to(&pre), related_to(&pre)];
    let error = store
        .create_task(params)
        .expect_err("repeated non-prerequisite relations must stay refused");
    let message = error.to_string();
    assert!(
        message.contains("duplicate task relation"),
        "refusal names the duplicate edge, got: {message}"
    );
    assert!(
        message.contains("once"),
        "refusal tells the caller to supply each relation once, got: {message}"
    );
}
