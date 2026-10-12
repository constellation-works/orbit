//! A pull that names the work its host has room for is granted only a task
//! inside that list; the rest stay in the owner's backlog as
//! `capacity_unavailable`, and a pull naming no list is unchanged.

use super::*;
use orbit_store::contracts::{AdmissionWorkFit, AdmissionWorkKey, TaskDocumentUpdateParams};

/// A backlog task with `tags` and `complexity`, on its own file so no
/// footprint holds it back.
fn task(owner: &Coordinated, title: &str, tags: &[&str], complexity: TaskComplexity) -> String {
    let path = format!("src/{title}.rs");
    let task = owner.create_task_in(title, &[path.as_str()]);
    owner
        .backends
        .task
        .document
        .update_task_document(
            &task.id,
            TaskDocumentUpdateParams {
                actor: "codex".into(),
                tags: Some(tags.iter().map(|tag| (*tag).to_string()).collect()),
                complexity: Some(complexity),
                ..Default::default()
            },
        )
        .unwrap();
    task.id
}

fn fitting(request: &str, keys: &[(&str, &str)], templates: &[&str]) -> AdmissionRequest {
    AdmissionRequest {
        admissible: Some(AdmissionWorkFit {
            keys: keys
                .iter()
                .map(|(work_class, complexity)| AdmissionWorkKey {
                    work_class: (*work_class).into(),
                    complexity: Some((*complexity).into()),
                })
                .collect(),
            templates: templates.iter().map(|name| (*name).to_string()).collect(),
        }),
        ..owner_request(request)
    }
}

fn claimed(receipt: &AdmissionReceipt) -> Option<String> {
    receipt
        .claim
        .as_ref()
        .and(receipt.task.as_ref())
        .map(|task| task.id.clone())
}

fn unfit(receipt: &AdmissionReceipt) -> HashSet<String> {
    receipt
        .capacity_unavailable
        .iter()
        .map(|diagnostic| diagnostic.task_id.clone())
        .collect()
}

#[test]
fn a_pull_is_granted_only_work_its_host_has_room_for() {
    if !isolated("work_fit::a_pull_is_granted_only_work_its_host_has_room_for") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let change = task(&owner, "change", &[], TaskComplexity::Hard);
    let no_diff = task(&owner, "sweep", &["no-diff-expected"], TaskComplexity::Low);
    let template = task(&owner, "audit", &["auto-task:audit"], TaskComplexity::Low);

    let receipt = owner.pull(&fitting("hard-room", &[("change", "low")], &[]));
    assert_eq!(
        claimed(&receipt),
        None,
        "no backlog task is inside the list"
    );
    assert_eq!(
        unfit(&receipt),
        HashSet::from([change.clone(), no_diff.clone(), template.clone()]),
        "each task left out is reported as kept off for capacity"
    );
    assert!(owner.claims().is_empty());

    let receipt = owner.pull(&fitting("no-diff-room", &[("no-diff", "low")], &[]));
    assert_eq!(claimed(&receipt), Some(no_diff));

    // A template task fits by its template, not by its class and complexity.
    let receipt = owner.pull(&fitting("template-room", &[("change", "low")], &["audit"]));
    assert_eq!(claimed(&receipt), Some(template));

    // Without a list, selection is as it was.
    let receipt = owner.pull(&owner_request("no-list"));
    assert_eq!(claimed(&receipt), Some(change));
    assert!(receipt.capacity_unavailable.is_empty());
}
