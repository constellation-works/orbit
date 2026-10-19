//! ORB-16938: a codebase scan read every task's history inside the commit
//! boundary, taking both commit locks and recording a holder for each, and
//! read the whole bundle to do it. A history or comments read must not queue
//! behind a partition section when there is nothing to recover, and must
//! still settle an interrupted commit or an incomplete write before it
//! exposes any row.

use std::io::Write;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_COMMENTS_FILE_NAME, TASK_EVENTS_FILE_NAME, TaskCommentRowV2,
};

use super::super::apply::apply_fault::{self, Fault};
use super::claim_section::Owner;
use super::commit_replay::leases;
use crate::contracts::TaskHistoryStoreBackend;
use crate::driver::file::task_bundle::{PendingWriteGuard, append_jsonl_row};
use crate::repository::task::v2::TaskV2Store;

/// History events and comment messages, read through the store's surface.
fn read(tasks: &TaskV2Store, task_id: &str) -> (Vec<String>, Vec<String>) {
    let history = TaskHistoryStoreBackend::get_task_history(tasks, task_id)
        .expect("history")
        .expect("task");
    let comments = TaskHistoryStoreBackend::get_task_comments(tasks, task_id)
        .expect("comments")
        .expect("task");
    (
        history.into_iter().map(|entry| entry.event).collect(),
        comments
            .into_iter()
            .map(|comment| comment.message)
            .collect(),
    )
}

/// Criterion: with nothing to recover, a history or comments read takes no
/// commit lock, so it completes while another section holds the partition
/// exclusively.
#[test]
fn a_settled_history_read_does_not_wait_for_a_partition_section() {
    let owner = Owner::open(1);
    let task_id = owner.admit("held").claim.task_id;
    let tasks = Arc::clone(&owner.tasks);
    let (done, finished) = mpsc::channel();

    let read_during_hold = owner
        .boundary
        .with_admission(|| {
            let task_id = task_id.clone();
            std::thread::spawn(move || {
                let _ = done.send(read(&tasks, &task_id));
            });
            Ok(finished.recv_timeout(Duration::from_secs(10)))
        })
        .expect("admission section");

    let (history, _) =
        read_during_hold.expect("the read completes while the partition is held exclusively");
    assert!(!history.is_empty(), "the read returns the task's history");
}

/// Criterion: a read that finds a commit interrupted after its decision, or
/// a bundle write that died unfinished, settles it under the boundary first.
/// The interrupted bind's history shows exactly once and its lease and
/// journal row are gone; the unfinished write's rows are never exposed.
#[test]
fn a_history_read_settles_an_interrupted_commit_or_write_first() {
    let reference = Owner::open(1);
    let admitted = reference.admit("crash");
    let (before, _) = read(&reference.tasks, &admitted.claim.task_id);
    admitted.bind(&reference.boundary);
    let (expected, _) = read(&reference.tasks, &admitted.claim.task_id);
    assert!(expected.len() > before.len(), "a bind appends history");

    let owner = Owner::open(1);
    let admitted = owner.admit("crash");
    let task_id = admitted.claim.task_id.clone();
    apply_fault::set(Fault::AfterEvents);
    admitted
        .try_bind(&owner.boundary)
        .expect_err("the committer dies after appending its events");
    assert_eq!(
        leases(&owner).len(),
        1,
        "the dead committer leaves its lease"
    );

    let (history, comments) = read(&owner.tasks, &task_id);
    assert_eq!(
        history, expected,
        "interrupted commit: the read replays the bind's history exactly once"
    );
    assert!(leases(&owner).is_empty(), "the replay clears the lease");
    assert!(
        owner
            .boundary
            .store
            .unsettled_task_commit_journal(owner.boundary.workspace_id())
            .expect("journal")
            .is_empty(),
        "the replay settles the journal"
    );

    let bundle_dir = owner
        .boundary
        .bundle_store
        .bundle_path(&task_id)
        .expect("bundle path");
    let guard = PendingWriteGuard::begin(&bundle_dir).expect("begin write");
    // Whole, valid rows: only the write's record says they are not committed.
    let events = bundle_dir.join(TASK_EVENTS_FILE_NAME);
    let last_event = std::fs::read_to_string(&events)
        .expect("read events")
        .lines()
        .last()
        .expect("an event")
        .to_string();
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&events)
        .expect("open events");
    writeln!(log, "{last_event}").expect("append event");
    append_jsonl_row(
        &bundle_dir.join(TASK_COMMENTS_FILE_NAME),
        &TaskCommentRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            comment_id: "uncommitted".into(),
            at: chrono::Utc::now(),
            by: "codex".into(),
            body: "never committed".into(),
            operation_id: None,
        },
    )
    .expect("append comment");
    // The writer dies before it commits or aborts.
    std::mem::forget(guard);
    assert_eq!(
        read(&owner.tasks, &task_id),
        (expected, comments),
        "unfinished write: the read exposes only committed rows"
    );
}
