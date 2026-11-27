//! Display reads stay out of the task-commit boundary. Under drain load the
//! readiness, Jobs, board, task-detail and resource-profile reads each took
//! an ordinary boundary section, so every dashboard poll queued behind a
//! held task writer for seconds. With a writer holding the partition, the
//! observational reads answer the settled snapshot without waiting for it,
//! settling a commit or repairing the index; the coordinated reads still
//! wait. After the writer commits, the next observation sees the commit.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use orbit_common::fs::io::FileLockOptions;
use orbit_types::task::TaskStatus;

use super::claim_section::{Owner, lock_events_observed};
use crate::boundary::v2::TaskV2Store;
use crate::contract::*;
use crate::repository::task_index::{TaskRegistryStore, task_registry_path};

const PARTITION: &str = "orbit-test-123456";

const TAG: &str = "no-diff-expected";

/// Only a hang guard for the channel handshakes.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the coordinated control read waits for the held partition
/// before it reports the wait as a refused acquisition.
const COORDINATED_WAIT: Duration = Duration::from_millis(300);

/// Messages that show a read entered recovery, repaired the index or waited
/// on a lock.
const WAIT_OR_REPAIR: [&str; 3] = [
    "still waiting for advisory file lock; a holder may be hung",
    "task index freshness waiting for a bundle writer",
    "task index fallback",
];

/// Everything a display surface reads, from the observational routes.
#[derive(Debug, PartialEq)]
struct Observed {
    board: Vec<(String, TaskStatus, String, usize)>,
    detail: Option<(String, TaskStatus, usize)>,
    listed: Option<(String, TaskStatus, usize)>,
    labels: BTreeMap<String, String>,
    statuses: BTreeMap<String, TaskStatus>,
    tagged: BTreeSet<String>,
}

fn observe(tasks: &TaskV2Store, ids: &BTreeSet<String>, detail: &str) -> Observed {
    let pages = tasks
        .observe_task_row_pages(
            &[
                (
                    TaskListFilter {
                        statuses: Some(vec![TaskStatus::Backlog]),
                        ..TaskListFilter::default()
                    },
                    25,
                ),
                (TaskListFilter::default(), 25),
            ],
            None,
            TaskListHydration::Full,
        )
        .expect("observe board pages");
    let board = pages
        .iter()
        .flat_map(|page| page.items.iter())
        .map(|row| {
            (
                row.task.id.clone(),
                row.task.status,
                row.task.title.clone(),
                row.comments.len(),
            )
        })
        .collect();
    let listed = tasks
        .observe_listed_task_row(detail)
        .expect("observe listed row")
        .map(|row| (row.task.title, row.task.status, row.comments.len()));
    let detail = tasks
        .observe_task_row(detail)
        .expect("observe detail")
        .map(|row| (row.task.title, row.task.status, row.comments.len()));
    let labels = tasks
        .observe_task_envelopes_for_ids(ids)
        .expect("observe labels")
        .into_iter()
        .map(|envelope| (envelope.id, envelope.title))
        .collect();
    let statuses = tasks
        .observe_task_status_index()
        .expect("observe statuses")
        .into_iter()
        .filter(|(id, _)| ids.contains(id))
        .collect();
    let tagged = tasks
        .observe_task_ids_with_tag(TAG, ids)
        .expect("observe tags");
    Observed {
        board,
        detail,
        listed,
        labels,
        statuses,
        tagged,
    }
}

/// The same facts through the coordinated reads admission and mutation use.
fn coordinated(tasks: &TaskV2Store, ids: &BTreeSet<String>, detail: &str) -> Observed {
    let pages = tasks
        .query_task_row_pages(
            &[
                (
                    TaskListFilter {
                        statuses: Some(vec![TaskStatus::Backlog]),
                        ..TaskListFilter::default()
                    },
                    25,
                ),
                (TaskListFilter::default(), 25),
            ],
            None,
            TaskListHydration::Full,
        )
        .expect("coordinated board pages");
    Observed {
        board: pages
            .iter()
            .flat_map(|page| page.items.iter())
            .map(|row| {
                (
                    row.task.id.clone(),
                    row.task.status,
                    row.task.title.clone(),
                    row.comments.len(),
                )
            })
            .collect(),
        detail: tasks
            .get_task_row(detail, false)
            .expect("coordinated detail")
            .map(|row| (row.task.title, row.task.status, row.comments.len())),
        listed: tasks
            .get_task_row(detail, true)
            .expect("coordinated listed row")
            .map(|row| (row.task.title, row.task.status, row.comments.len())),
        labels: tasks
            .task_envelopes_for_ids(ids)
            .expect("coordinated labels")
            .into_iter()
            .map(|envelope| (envelope.id, envelope.title))
            .collect(),
        statuses: tasks
            .task_status_index()
            .expect("coordinated statuses")
            .into_iter()
            .filter(|(id, _)| ids.contains(id))
            .collect(),
        tagged: tasks.task_ids_with_tag(TAG, ids).expect("coordinated tags"),
    }
}

fn store_over(owner: &Owner, options: FileLockOptions) -> TaskV2Store {
    TaskV2Store::with_commit_boundary(
        TaskRegistryStore::open(&task_registry_path(owner.root.path())).expect("open registry"),
        PARTITION.into(),
        Arc::new(owner.boundary_with(options)),
    )
}

#[test]
fn display_reads_answer_the_settled_snapshot_while_a_writer_holds_the_partition() {
    let owner = Owner::open(3);
    let ids = owner
        .tasks
        .list_tasks()
        .expect("list")
        .into_iter()
        .map(|task| task.id)
        .collect::<BTreeSet<_>>();
    let first = ids.first().cloned().expect("a task");
    let last = ids.last().cloned().expect("a task");
    owner
        .tasks
        .update_task_document(
            &last,
            &TaskDocumentUpdateParams {
                actor: "human:fixture".into(),
                tags: Some(vec![TAG.into()]),
                ..TaskDocumentUpdateParams::default()
            },
        )
        .expect("tag a template");

    let reader = store_over(
        &owner,
        FileLockOptions {
            timeout: HANDSHAKE_TIMEOUT,
            ..FileLockOptions::default()
        },
    );
    let control = store_over(
        &owner,
        FileLockOptions {
            timeout: COORDINATED_WAIT,
            warn_after: COORDINATED_WAIT,
            ..FileLockOptions::default()
        },
    );
    let settled = coordinated(&reader, &ids, &first);
    assert_eq!(settled.tagged, BTreeSet::from([last.clone()]));
    assert_eq!(
        observe(&reader, &ids, &first),
        settled,
        "settled observation matches the coordinated reads"
    );

    let writer = owner.boundary_with(FileLockOptions {
        timeout: HANDSHAKE_TIMEOUT,
        ..FileLockOptions::default()
    });
    let (held_tx, held) = mpsc::sync_channel(1);
    let (release_tx, release) = mpsc::sync_channel::<()>(1);
    std::thread::scope(|scope| {
        let holder = scope.spawn(move || {
            writer
                .with_admission(|| {
                    held_tx.send(()).expect("signal the held writer");
                    release
                        .recv_timeout(HANDSHAKE_TIMEOUT)
                        .expect("release the writer");
                    Ok(())
                })
                .expect("held admission")
        });
        held.recv_timeout(HANDSHAKE_TIMEOUT)
            .expect("the writer holds the partition");

        // Before: the coordinated reads the display surfaces used wait on the
        // held writer and give up after the control's short deadline.
        for (read, result) in [
            (
                "task_envelopes_for_ids",
                control.task_envelopes_for_ids(&ids).map(|_| ()),
            ),
            (
                "task_ids_with_tag",
                control.task_ids_with_tag(TAG, &ids).map(|_| ()),
            ),
            (
                "get_task_row",
                control.get_task_row(&first, true).map(|_| ()),
            ),
            (
                "query_task_row_pages",
                control
                    .query_task_row_pages(
                        &[(TaskListFilter::default(), 25)],
                        None,
                        TaskListHydration::Full,
                    )
                    .map(|_| ()),
            ),
        ] {
            assert!(
                result.is_err(),
                "the coordinated {read} waits for the held writer"
            );
        }

        // After: every observational read answers at once, from the settled
        // snapshot, with no lock wait, recovery or index repair.
        let (observed, events) = lock_events_observed(|| observe(&reader, &ids, &first), |_| {});
        let waited = events
            .iter()
            .filter(|event| {
                event
                    .get("message")
                    .is_some_and(|message| WAIT_OR_REPAIR.contains(&message.as_str()))
            })
            .collect::<Vec<_>>();
        assert!(
            waited.is_empty(),
            "display reads neither wait nor repair: {waited:?}"
        );
        assert_eq!(observed, settled, "the held writer changes nothing seen");

        release_tx.send(()).expect("release the writer");
        holder.join().expect("holder joined");
    });

    // The writer's commit, then the next observation sees it.
    owner
        .tasks
        .update_task_history(
            &first,
            &TaskHistoryUpdateParams {
                actor: "human:fixture".into(),
                status: Some(TaskStatus::Blocked),
                append_comments: vec![orbit_types::task::TaskComment {
                    at: chrono::Utc::now(),
                    by: "human:fixture".into(),
                    message: "held".into(),
                    operation_id: None,
                }],
                ..TaskHistoryUpdateParams::default()
            },
        )
        .expect("commit");
    let committed = coordinated(&reader, &ids, &first);
    assert_eq!(committed.statuses[&first], TaskStatus::Blocked);
    assert_eq!(
        committed.detail.as_ref().map(|detail| detail.2),
        Some(settled.detail.as_ref().map_or(0, |detail| detail.2) + 1)
    );
    assert_eq!(
        observe(&reader, &ids, &first),
        committed,
        "the next observation sees the committed facts"
    );
}
