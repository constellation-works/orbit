use super::*;
use std::sync::Barrier;

fn create_tasks(store: &WorkspaceTaskBackends, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            store
                .task
                .create_task(create_params(&format!("Task {index}"), TaskStatus::Backlog))
                .expect("create task")
                .id
        })
        .collect()
}

/// Same-task concurrency: the per-task lock must serialize whole updates, so
/// every appended comment survives instead of racing writers overwriting each
/// other's view of the comment sequence.
#[test]
fn parallel_updates_to_one_task_keep_every_appended_comment() {
    if !support::isolated("concurrency::parallel_updates_to_one_task_keep_every_appended_comment") {
        return;
    }
    const WRITERS: usize = 6;
    const COMMENTS_PER_WRITER: usize = 5;

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = create_tasks(&store, 1).remove(0);
    let created_comments = store
        .history
        .get_task_comments(&id)
        .expect("get comments")
        .expect("task exists")
        .len();

    std::thread::scope(|scope| {
        for writer in 0..WRITERS {
            let store = &store;
            let id = &id;
            scope.spawn(move || {
                for round in 0..COMMENTS_PER_WRITER {
                    store
                        .history
                        .update_task_history(
                            id,
                            TaskHistoryUpdateParams {
                                actor: "codex:gpt-5.5".to_string(),
                                append_comments: vec![TaskComment {
                                    at: Utc::now(),
                                    by: format!("writer-{writer}"),
                                    message: format!("writer {writer} comment {round}"),
                                    operation_id: None,
                                }],
                                ..Default::default()
                            },
                        )
                        .unwrap_or_else(|err| panic!("history update failed: {err}"));
                }
            });
        }
    });

    let comments = store
        .history
        .get_task_comments(&id)
        .expect("get comments")
        .expect("task exists");
    assert_eq!(
        comments.len(),
        created_comments + WRITERS * COMMENTS_PER_WRITER,
        "no concurrent comment may be lost"
    );
}

/// Force two guarded writers to share one revision; only one can commit its
/// status and receipt. Reopening the store simulates a lost response/restart.
#[test]
fn desktop_concurrent_status_receipts_are_atomic_and_durable() {
    if !support::isolated("concurrency::desktop_concurrent_status_receipts_are_atomic_and_durable")
    {
        return;
    }
    use orbit_store::contracts::{AtomicTaskMutationOutcome, DesktopTaskMutationParams};
    let temp = TempDir::new().unwrap();
    let store = store(&temp);
    let id = store
        .task
        .create_task(create_params("Guarded race", TaskStatus::Proposed))
        .unwrap()
        .id;
    let revision = store.task.desktop_task_revision(&id).unwrap();
    let request = DesktopTaskMutationParams {
        actor: "codex".into(),
        request_id: "approval".into(),
        payload_digest: "a".repeat(64),
        expected_revision: revision,
        fields: Default::default(),
        crew_source: None,
        comment: None,
        status: Some(TaskStatus::Backlog),
        append_history: Vec::new(),
    };
    let mut competing = request.clone();
    competing.request_id = "competing".into();
    competing.payload_digest = "b".repeat(64);
    competing.status = Some(TaskStatus::Blocked);
    let barrier = Barrier::new(2);
    let outcomes = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            store
                .task
                .apply_desktop_task_mutation(&id, &request)
                .unwrap()
        });
        let b = scope.spawn(|| {
            barrier.wait();
            store
                .task
                .apply_desktop_task_mutation(&id, &competing)
                .unwrap()
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert!(matches!(
        outcomes,
        (
            AtomicTaskMutationOutcome::Applied,
            AtomicTaskMutationOutcome::Stale
        ) | (
            AtomicTaskMutationOutcome::Stale,
            AtomicTaskMutationOutcome::Applied
        )
    ));
    let winner = if outcomes.0 == AtomicTaskMutationOutcome::Applied {
        request
    } else {
        competing
    };
    drop(store);
    let reopened = super::store(&temp);
    let committed = reopened.task.read_desktop_task(&id).unwrap();
    assert_eq!(committed.task.status, winner.status.unwrap());
    assert_eq!(
        reopened
            .task
            .apply_desktop_task_mutation(&id, &winner)
            .unwrap(),
        AtomicTaskMutationOutcome::AlreadyApplied
    );
    assert_eq!(
        reopened.task.desktop_task_revision(&id).unwrap(),
        committed.revision
    );
    let mut changed = winner;
    changed.payload_digest = "c".repeat(64);
    assert!(
        reopened
            .task
            .apply_desktop_task_mutation(&id, &changed)
            .is_err()
    );
    assert_eq!(
        reopened.task.desktop_task_revision(&id).unwrap(),
        committed.revision
    );
}
