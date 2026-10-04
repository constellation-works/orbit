//! ORB-10988 / F2026-07-119: concurrent writes to one task serialize whole
//! updates, and a reader never observes a half-published transition.

use std::sync::Barrier;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::*;

fn create_tasks(store: &TaskV2Store, count: usize) -> Vec<String> {
    (0..count)
        .map(|index| {
            store
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
    const WRITERS: usize = 6;
    const COMMENTS_PER_WRITER: usize = 5;

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = create_tasks(&store, 1).remove(0);
    let created_comments = store
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
                        .update_task_history(
                            id,
                            &TaskHistoryUpdateParams {
                                actor: "codex:gpt-5.5".to_string(),
                                append_comments: vec![TaskComment {
                                    at: Utc::now(),
                                    by: format!("writer-{writer}"),
                                    message: format!("writer {writer} comment {round}"),
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
        .get_task_comments(&id)
        .expect("get comments")
        .expect("task exists");
    assert_eq!(
        comments.len(),
        created_comments + WRITERS * COMMENTS_PER_WRITER,
        "no concurrent comment may be lost"
    );
}

/// ORB-11349: the transition row and the envelope that agrees with it are two
/// files, so `update_task_history` is only a consistent unit to a reader that
/// waits for the writer. Build exactly that half-published state — hold the
/// task's write lock, append the event, and stall before republishing the
/// envelope — and require a reader that arrives inside the window to observe
/// the settled transition rather than the mixed pair.
#[test]
fn a_reader_never_observes_a_transition_between_its_event_and_its_envelope() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = create_tasks(&store, 2).remove(0);

    let event_appended = Barrier::new(2);
    let envelope_published = AtomicBool::new(false);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            store
                .with_task_lock(&id, || {
                    let bundle = store.bundle_store.read_bundle(&id).expect("read bundle");
                    store
                        .bundle_store
                        .append_event(&id, &transition_event(&bundle))
                        .expect("append transition event");
                    event_appended.wait();
                    // Wide enough that a reader taking no lock lands inside the
                    // window every run, not just under load.
                    std::thread::sleep(Duration::from_millis(300));
                    let mut envelope = bundle.envelope.clone();
                    envelope.status = TaskStatus::InProgress;
                    envelope.updated_at = Utc::now();
                    store
                        .bundle_store
                        .rewrite_envelope(&id, &envelope)
                        .expect("publish envelope");
                    envelope_published.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .expect("half-published transition");
        });

        event_appended.wait();
        let task = store
            .get_task(&id)
            .expect("a half-published transition must not read as corruption")
            .expect("task exists");
        assert!(
            envelope_published.load(Ordering::SeqCst),
            "the read must have waited for the writer to publish the envelope"
        );
        assert_eq!(
            task.status,
            TaskStatus::InProgress,
            "the read must see the settled transition, not the pre-transition envelope"
        );

        let listed = store
            .list_tasks()
            .expect("a half-published transition must not fail the listing");
        assert_eq!(listed.len(), 2, "no task may drop out of a listing");
    });
}

/// A status transition for `bundle`'s task that its envelope does not yet
/// reflect — the exact row `update_task_history` appends before republishing.
fn transition_event(bundle: &TaskBundleV2) -> TaskEventRowV2 {
    TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: format!("EV-{:04}", bundle.events.len() + 1),
        at: Utc::now(),
        by: "codex:gpt-5.5".to_string(),
        event_type: "status_changed".to_string(),
        note: None,
        from_status: Some(bundle.envelope.status),
        to_status: Some(TaskStatus::InProgress),
    }
}
