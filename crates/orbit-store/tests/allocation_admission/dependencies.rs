//! Dependency satisfaction through the serialized owner admission boundary.

use super::*;
use orbit_store::contracts::{TaskDocumentUpdateParams, TaskHistoryUpdateParams, TaskListFilter};

fn owners(root: &Path, cross_workspace: bool) -> (Coordinated, Coordinated) {
    let owner = Coordinated::open_with_fingerprint(root, PARTITION_ID, "repo");
    let dependency_owner = Coordinated::open_with_fingerprint(
        root,
        if cross_workspace {
            "ws_dependency"
        } else {
            PARTITION_ID
        },
        "repo",
    );
    (owner, dependency_owner)
}

fn transition(owner: &Coordinated, id: &str, statuses: &[TaskStatus]) {
    for status in statuses {
        owner
            .backends
            .task
            .history
            .update_task_history(
                id,
                TaskHistoryUpdateParams {
                    actor: "codex".into(),
                    status: Some(*status),
                    ..Default::default()
                },
            )
            .unwrap();
    }
}

fn dependent(owner: &Coordinated, dependency: &str) -> String {
    let task = owner.create_task("dependent");
    owner
        .backends
        .task
        .document
        .update_task_document(
            &task.id,
            TaskDocumentUpdateParams {
                actor: "codex".into(),
                dependencies: Some(vec![dependency.into()]),
                ..Default::default()
            },
        )
        .unwrap();
    task.id
}

#[test]
fn completed_archived_dependencies_are_claimed_locally_and_across_workspaces() {
    if !isolated(
        "dependencies::completed_archived_dependencies_are_claimed_locally_and_across_workspaces",
    ) {
        return;
    }
    for cross_workspace in [false, true] {
        let root = TempDir::new().unwrap();
        let (owner, dependency_owner) = owners(root.path(), cross_workspace);
        let prerequisite = dependency_owner.create_task("completed prerequisite");
        transition(
            &dependency_owner,
            &prerequisite.id,
            &[TaskStatus::Done, TaskStatus::Archived],
        );
        let dependents = [
            dependent(&owner, &prerequisite.id),
            dependent(&owner, &prerequisite.id),
        ];
        let ready = owner
            .backends
            .task
            .task
            .query_task_rows(
                &TaskListFilter {
                    statuses: Some(vec![TaskStatus::Backlog]),
                    ..Default::default()
                },
                10,
                Some(&|task, statuses| {
                    task.dependencies()
                        .iter()
                        .all(|id| statuses.get(id) == Some(&TaskStatus::Done))
                }),
            )
            .unwrap();
        assert_eq!(ready.items.len(), dependents.len());
        let receipt = owner.pull(&owner_request("completed-archive"));
        let claimed = receipt
            .claim
            .as_ref()
            .expect("completed archive permits a claim");
        assert!(dependents.contains(&claimed.task_id));
        assert_eq!(
            receipt.queue_depth,
            ready.items.len() - 1,
            "the receipt counts ready tasks remaining after its claim, cross={cross_workspace}"
        );
        assert!(receipt.invalid_candidates.is_empty());
        assert_eq!(owner.task_status(&claimed.task_id), TaskStatus::InProgress);
        assert_eq!(owner.claims().len(), 1);
        assert_eq!(owner.active_reservations().len(), 1);
        // Both dependents share a file, so the remaining ready task waits
        // for the first claim's footprint while still contributing to depth.
        let waiting = owner.pull(&owner_request("completed-archive-waiting"));
        assert_eq!(waiting.queue_depth, 1);
        assert!(waiting.claim.is_none());
        assert!(waiting.invalid_candidates.is_empty());
        assert_eq!(waiting.deferred_conflicts.len(), 1);
        assert!(dependents.contains(&waiting.deferred_conflicts[0].task_id));
        assert_eq!(
            owner.task_status(&waiting.deferred_conflicts[0].task_id),
            TaskStatus::Backlog
        );
        assert_eq!(
            dependency_owner.task_status(&prerequisite.id),
            TaskStatus::Archived,
            "only the dependency projection changes"
        );
    }
}

#[test]
fn unsatisfied_dependencies_never_create_claims_locally_or_across_workspaces() {
    if !isolated(
        "dependencies::unsatisfied_dependencies_never_create_claims_locally_or_across_workspaces",
    ) {
        return;
    }
    let cases: &[(&str, &[TaskStatus])] = &[
        ("abandoned", &[TaskStatus::Archived]),
        (
            "reopened",
            &[TaskStatus::Done, TaskStatus::Backlog, TaskStatus::Archived],
        ),
        ("rejected", &[TaskStatus::Rejected]),
        ("missing", &[TaskStatus::Done]),
        ("empty-history", &[TaskStatus::Done, TaskStatus::Archived]),
    ];
    for cross_workspace in [false, true] {
        for (case, statuses) in cases {
            let root = TempDir::new().unwrap();
            let (owner, dependency_owner) = owners(root.path(), cross_workspace);
            let prerequisite = dependency_owner.create_task(case);
            transition(&dependency_owner, &prerequisite.id, statuses);
            let dependent = dependent(&owner, &prerequisite.id);
            match *case {
                "missing" => {
                    assert!(
                        dependency_owner
                            .backends
                            .task
                            .task
                            .delete_task(&prerequisite.id)
                            .unwrap()
                    );
                }
                "empty-history" => {
                    let bundle = dependency_owner
                        .registry
                        .canonical_task_bundle_path(
                            dependency_owner.backends.commit_boundary.workspace_id(),
                            &prerequisite.id,
                        )
                        .unwrap();
                    std::fs::write(bundle.join(TASK_EVENTS_FILE_NAME), b"").unwrap();
                }
                _ => {}
            }
            let before = owner.backends.task.task.get_task(&dependent).unwrap();
            let history = owner
                .backends
                .task
                .history
                .get_task_history(&dependent)
                .unwrap();
            let receipt = owner.pull(&owner_request(case));
            assert_eq!(receipt.queue_depth, 0, "{case}, cross={cross_workspace}");
            assert!(receipt.claim.is_none());
            assert!(receipt.task.is_none());
            assert_eq!(receipt.invalid_candidates.len(), 1);
            assert_eq!(receipt.invalid_candidates[0].task_id, dependent);
            assert_eq!(receipt.invalid_candidates[0].blocked_by, [prerequisite.id]);
            assert!(owner.claims().is_empty());
            assert!(owner.active_reservations().is_empty());
            assert_eq!(
                owner.backends.task.task.get_task(&dependent).unwrap(),
                before
            );
            assert_eq!(
                owner
                    .backends
                    .task
                    .history
                    .get_task_history(&dependent)
                    .unwrap(),
                history
            );
        }
    }
}

#[test]
fn unreadable_dependency_history_aborts_admission_without_claiming_or_mutating() {
    if !isolated(
        "dependencies::unreadable_dependency_history_aborts_admission_without_claiming_or_mutating",
    ) {
        return;
    }
    for cross_workspace in [false, true] {
        for missing in [false, true] {
            let root = TempDir::new().unwrap();
            let (owner, dependency_owner) = owners(root.path(), cross_workspace);
            let prerequisite = dependency_owner.create_task("unreadable history");
            transition(
                &dependency_owner,
                &prerequisite.id,
                &[TaskStatus::Done, TaskStatus::Archived],
            );
            let dependent = dependent(&owner, &prerequisite.id);
            let before = owner.backends.task.task.get_task(&dependent).unwrap();
            let history = owner
                .backends
                .task
                .history
                .get_task_history(&dependent)
                .unwrap();
            let bundle = dependency_owner
                .registry
                .canonical_task_bundle_path(
                    dependency_owner.backends.commit_boundary.workspace_id(),
                    &prerequisite.id,
                )
                .unwrap();
            let events = bundle.join(TASK_EVENTS_FILE_NAME);
            if missing {
                std::fs::rename(&events, bundle.join("retained-events.jsonl")).unwrap();
            } else {
                // An invalid interior row cannot be mistaken for a torn tail.
                std::fs::write(&events, b"{invalid json\n{}\n").unwrap();
            }
            let outcome = owner.try_pull(&owner_request("unreadable-history"));
            assert!(
                matches!(outcome, Err(OrbitError::TaskBundleCorrupt { .. })),
                "unreadable history must refuse admission: {outcome:?}"
            );
            assert!(owner.claims().is_empty());
            assert!(owner.active_reservations().is_empty());
            assert_eq!(
                owner.backends.task.task.get_task(&dependent).unwrap(),
                before
            );
            assert_eq!(
                owner
                    .backends
                    .task
                    .history
                    .get_task_history(&dependent)
                    .unwrap(),
                history
            );
        }
    }
}
