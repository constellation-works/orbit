//! ORB-10988 / F2026-07-119: a write to one task must never fail a read or a
//! write of another.
//!
//! The registry binding list is a snapshot and a bundle is a directory, so an
//! unrelated create or delete is observable to a reader as a bundle that is
//! missing or half there. These tests pin both halves of the rule: transient
//! states are skipped, genuine corruption still fails fast.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use super::*;
use crate::driver::file::task_bundle::task_bundle_lock_sentinel_path;

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

fn document_update(actor: &str, summary: &str) -> TaskDocumentUpdateParams {
    TaskDocumentUpdateParams {
        actor: actor.to_string(),
        execution_summary: Some(summary.to_string()),
        ..Default::default()
    }
}

/// A bundle removed between the registry snapshot and the read — the window
/// `delete_bundle` opens by unregistering and unlinking as two steps — used to
/// fail the whole listing. It must now cost only that one task.
#[test]
fn listing_survives_a_bundle_removed_under_a_live_registry_binding() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let ids = create_tasks(&store, 3);

    let vanished = store
        .bundle_store
        .bundle_path(&ids[1])
        .expect("bundle path");
    std::fs::remove_dir_all(&vanished).expect("remove bundle out of band");

    let listed: Vec<String> = store
        .list_tasks()
        .expect("an unrelated task's removal must not fail the listing")
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert_eq!(listed.len(), 2, "listed: {listed:?}");
    assert!(!listed.contains(&ids[1]), "listed: {listed:?}");
    assert!(listed.contains(&ids[0]) && listed.contains(&ids[2]));

    assert_eq!(
        store
            .bundle_store
            .list_bundles()
            .expect("list bundles")
            .len(),
        2
    );
}

/// An incomplete bundle whose create/delete lock sentinel is present is a
/// writer's work in progress, not damage: skip it and serve every other task.
#[test]
fn listing_skips_an_incomplete_bundle_held_by_the_lock_sentinel() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let ids = create_tasks(&store, 2);

    let in_flight = store
        .bundle_store
        .bundle_path(&ids[0])
        .expect("bundle path");
    let sentinel = task_bundle_lock_sentinel_path(&in_flight).expect("sentinel path");
    std::fs::write(&sentinel, b"").expect("hold the sentinel");
    std::fs::remove_file(in_flight.join("description.md")).expect("truncate publication");

    let listed: Vec<String> = store
        .list_tasks()
        .expect("an in-flight bundle must not fail the listing")
        .into_iter()
        .map(|task| task.id)
        .collect();
    assert_eq!(listed, vec![ids[1].clone()]);
}

/// The tolerance is narrow on purpose: a bundle that is neither held by a
/// writer nor gone is damaged, and damage must still surface loudly rather
/// than quietly shrinking every listing.
#[test]
fn listing_still_fails_fast_on_a_settled_corrupt_bundle() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let ids = create_tasks(&store, 2);

    let corrupt = store
        .bundle_store
        .bundle_path(&ids[0])
        .expect("bundle path");
    std::fs::remove_file(corrupt.join("description.md")).expect("damage bundle");

    let err = store
        .list_tasks()
        .expect_err("a settled, damaged bundle must not be silently skipped");
    assert!(
        matches!(err, OrbitError::TaskBundleCorrupt { ref task_id, .. } if *task_id == ids[0]),
        "expected corruption for {}, got {err}",
        ids[0]
    );
}

/// The reported failure shape: parallel writes to *distinct* tasks racing the
/// index validation and rebuild that every listing performs. Serially these
/// same calls always succeeded; concurrently they transiently failed.
#[test]
fn parallel_updates_to_distinct_tasks_never_fail_a_concurrent_listing() {
    const TASKS: usize = 8;
    const ROUNDS: usize = 12;

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let ids = create_tasks(&store, TASKS);
    let listings = AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for (index, id) in ids.iter().enumerate() {
            let store = &store;
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    store
                        .update_task_document(
                            id,
                            &document_update("codex:gpt-5.5", &format!("writer {index} @{round}")),
                        )
                        .unwrap_or_else(|err| {
                            panic!("update of {id} failed under concurrency: {err}")
                        });
                }
            });
        }
        for _ in 0..4 {
            let store = &store;
            let listings = &listings;
            scope.spawn(move || {
                for _ in 0..(ROUNDS * TASKS) {
                    let tasks = store
                        .list_tasks()
                        .unwrap_or_else(|err| panic!("listing failed under concurrency: {err}"));
                    assert_eq!(tasks.len(), TASKS, "no task may drop out of a listing");
                    let page = store
                        .query_task_rows(&Default::default(), 3, None)
                        .unwrap_or_else(|err| {
                            panic!("bounded listing failed under concurrency: {err}")
                        });
                    assert_eq!(page.items.len(), 3);
                    listings.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });

    assert!(listings.load(Ordering::Relaxed) > 0);
    for (index, id) in ids.iter().enumerate() {
        let task = store.get_task(id).expect("get task").expect("task exists");
        assert_eq!(
            task.execution_summary,
            format!("writer {index} @{}", ROUNDS - 1),
            "every writer's last write must be durable"
        );
    }
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

/// The tolerance stays narrow: once no writer holds the bundle and no pending
/// write record exists, an event log that disagrees with the envelope is real
/// damage and must still surface.
#[test]
fn a_settled_event_and_envelope_mismatch_is_still_corruption() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let id = create_tasks(&store, 2).remove(0);

    let bundle = store.bundle_store.read_bundle(&id).expect("read bundle");
    store
        .bundle_store
        .append_event(&id, &transition_event(&bundle))
        .expect("append transition event");

    let err = store
        .get_task(&id)
        .expect_err("a settled status mismatch must not be silently tolerated");
    assert!(
        matches!(err, OrbitError::TaskBundleCorrupt { ref task_id, .. } if *task_id == id),
        "expected corruption for {id}, got {err}"
    );
    let err = store
        .list_tasks()
        .expect_err("a settled status mismatch must not be silently skipped");
    assert!(
        matches!(err, OrbitError::TaskBundleCorrupt { ref task_id, .. } if *task_id == id),
        "expected corruption for {id}, got {err}"
    );
}

/// Every lifecycle write reads the bundle it is about to modify from inside
/// its own write lock, and some read every *other* task too. The read lock
/// must re-enter the write lock it is nested in rather than block on it.
#[test]
fn a_full_read_inside_the_write_lock_does_not_deadlock() {
    let temp = Arc::new(TempDir::new().expect("tempdir"));
    let store = Arc::new(store(&temp));
    let ids = create_tasks(&store, 3);

    // Detached, not scoped: a regression here deadlocks, and a scoped thread
    // would take the whole suite down with it on join.
    let finished = run_within(Duration::from_secs(20), move || {
        let _temp = temp;
        store
            .with_task_lock(&ids[0], || {
                store.bundle_store.read_bundle(&ids[0])?;
                store.get_task(&ids[0])?;
                store.list_tasks().map(|tasks| tasks.len())
            })
            .expect("a nested read must not deadlock against its own write lock")
    });
    assert_eq!(
        finished,
        Some(3),
        "a read nested inside this task's own write lock must re-enter it"
    );
}

/// The lock is per bundle, so a writer stalled on one task must not stop reads
/// of any other. This is the property the incident actually broke: one task's
/// transition failed an unrelated task's admission.
#[test]
fn a_stalled_writer_on_one_task_does_not_block_reads_of_another() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let ids = create_tasks(&store, 2);

    let holding = Barrier::new(2);
    let released = AtomicBool::new(false);

    std::thread::scope(|scope| {
        scope.spawn(|| {
            store
                .with_task_lock(&ids[0], || {
                    holding.wait();
                    std::thread::sleep(Duration::from_millis(300));
                    released.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .expect("hold the write lock");
        });

        holding.wait();
        let other = store
            .get_task(&ids[1])
            .expect("read of an unrelated task")
            .expect("task exists");
        assert_eq!(other.id, ids[1]);
        assert!(
            !released.load(Ordering::SeqCst),
            "an unrelated task's read must not wait for this writer"
        );
    });
}

/// The reported failure shape at status-transition scale: writers flipping
/// their own task's status while readers assemble whole bundles.
#[test]
fn parallel_status_transitions_never_fail_a_concurrent_read() {
    const TASKS: usize = 6;
    const ROUNDS: usize = 10;

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let ids = create_tasks(&store, TASKS);

    std::thread::scope(|scope| {
        for id in &ids {
            let store = &store;
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    let status = if round % 2 == 0 {
                        TaskStatus::InProgress
                    } else {
                        TaskStatus::Backlog
                    };
                    store
                        .update_task_history(
                            id,
                            &TaskHistoryUpdateParams {
                                actor: "codex:gpt-5.5".to_string(),
                                status: Some(status),
                                ..Default::default()
                            },
                        )
                        .unwrap_or_else(|err| panic!("transition of {id} failed: {err}"));
                }
            });
        }
        for _ in 0..4 {
            let store = &store;
            let ids = &ids;
            scope.spawn(move || {
                for _ in 0..(ROUNDS * TASKS) {
                    let tasks = store
                        .list_tasks()
                        .unwrap_or_else(|err| panic!("listing failed under transitions: {err}"));
                    assert_eq!(tasks.len(), TASKS, "no task may drop out of a listing");
                    for id in ids {
                        store
                            .get_task(id)
                            .unwrap_or_else(|err| panic!("read of {id} failed: {err}"))
                            .expect("task exists");
                    }
                }
            });
        }
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

/// Run `op` on its own thread, returning `None` if it has not finished within
/// `budget`. A deadlock regression then fails this one test instead of hanging
/// the whole suite until CI kills it.
fn run_within<T, F>(budget: Duration, op: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(op());
    });
    receiver.recv_timeout(budget).ok()
}
