//! Generated-index observation and publication through composed task backends.

use std::sync::Mutex;

use orbit_store::compose::{WorkspaceTaskBackends, workspace_observational_backends};
use orbit_store::contracts::{TaskDocumentUpdateParams, TaskListFilter};
use serde_json::Value;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::*;

fn observer(root: &Path) -> WorkspaceTaskBackends {
    workspace_observational_backends(
        TaskRegistryStore::open_read_only(&task_registry_path(root)).unwrap(),
        PARTITION_ID.into(),
        Store::open_read_only(&root.join("state.sqlite")).unwrap(),
    )
    .unwrap()
    .task
}

fn events(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn read_only_fallback_does_not_suppress_the_next_writer_repair() {
    if !isolated("task_index::read_only_fallback_does_not_suppress_the_next_writer_repair") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let created = owner.create_task("Index observation");
    let sql = rusqlite::Connection::open(task_registry_path(root.path())).unwrap();
    sql.execute("UPDATE task_bundle_index SET updated_at = 'stale'", [])
        .unwrap();
    let read_only = observer(root.path());
    let log = root.path().join("index.jsonl");
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(Mutex::new(std::fs::File::create(&log).unwrap()))
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);

    for _ in 0..2 {
        let tasks = read_only.task.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, created.id);
    }
    assert_eq!(
        sql.query_row("SELECT updated_at FROM task_bundle_index", [], |row| {
            row.get::<_, String>(0)
        })
        .unwrap(),
        "stale",
        "observation must leave the generated index untouched"
    );
    let reads = events(&log);
    let scans: Vec<_> = reads
        .iter()
        .filter(|event| event["fields"]["path"] == "bundle_scan")
        .collect();
    assert_eq!(scans.len(), 2, "both stale reads must use bundle fallback");
    assert!(
        scans
            .iter()
            .all(|event| event["fields"]["repair_admitted"] == false)
    );
    assert!(reads.iter().all(|event| event["level"] != "WARN"));

    // Same process, registry and workspace: a read-only gate failure would
    // suppress this repair over the unchanged envelopes (ORB-14851).
    assert_eq!(owner.backends.task.task.list_tasks().unwrap().len(), 1);
    assert_eq!(
        owner
            .registry
            .indexed_task_versions_for_workspace(PARTITION_ID)
            .unwrap()[&created.id],
        created.updated_at.to_rfc3339()
    );
    assert_eq!(read_only.task.list_tasks().unwrap().len(), 1);
    let repaired = events(&log);
    assert_eq!(
        repaired
            .iter()
            .filter(|event| event["fields"]["repair_admitted"] == true)
            .count(),
        1,
        "a writer must still receive its first repair ticket"
    );
    assert_eq!(repaired.last().unwrap()["fields"]["path"], "index");
    assert!(repaired.iter().all(|event| event["level"] != "WARN"));
}

/// Complete one supported write after the reader captured its index rows,
/// before it probes the envelope. This forces the production interleaving
/// without sleeps or exposing private store methods.
struct WriteAtSnapshot(Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl<S: tracing::Subscriber> Layer<S> for WriteAtSnapshot {
    fn on_event(&self, event: &tracing::Event<'_>, _context: Context<'_, S>) {
        if event.metadata().target() == "orbit.store.task_query"
            && event.metadata().fields().field("indexed_tasks").is_some()
        {
            let write = self.0.lock().unwrap().take();
            if let Some(write) = write {
                write();
            }
        }
    }
}

#[test]
fn completed_write_during_freshness_probe_keeps_selection_indexed() {
    if !isolated("task_index::completed_write_during_freshness_probe_keeps_selection_indexed") {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let created = owner.create_task("Concurrent index publication");
    let read_only = observer(root.path());
    let log = root.path().join("index.jsonl");
    let document = owner.backends.task.document.clone();
    let id = created.id.clone();
    let subscriber = Registry::default()
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(Mutex::new(std::fs::File::create(&log).unwrap())),
        )
        .with(WriteAtSnapshot(Mutex::new(Some(Box::new(move || {
            document
                .update_task_document(
                    &id,
                    TaskDocumentUpdateParams {
                        actor: "codex".into(),
                        priority: Some(TaskPriority::Low),
                        tags: Some(vec!["published".into(), "fresh".into()]),
                        ..Default::default()
                    },
                )
                .unwrap();
        })))));
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let tasks = read_only
        .task
        .list_tasks_by_tags(&["published".into(), "fresh".into()])
        .unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, created.id);
    assert_eq!(tasks[0].priority, TaskPriority::Low);
    let reads = events(&log);
    assert_eq!(
        reads
            .iter()
            .filter(|event| event["fields"]["indexed_tasks"].is_number())
            .count(),
        2
    );
    assert!(reads.iter().any(|event| event["fields"]["path"] == "index"));
    assert!(
        reads
            .iter()
            .all(|event| event["fields"]["path"] != "bundle_scan")
    );
    assert!(reads.iter().all(|event| event["level"] != "WARN"));
}

/// Envelope stamps the registry holds for `PARTITION_ID`; none before the
/// first is recorded, when the table does not exist yet.
fn recorded_stamps(sql: &rusqlite::Connection) -> i64 {
    let exists: bool = sql
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name = 'task_envelope_stamps')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    if !exists {
        return 0;
    }
    sql.query_row(
        "SELECT COUNT(*) FROM task_envelope_stamps WHERE workspace_id = ?1",
        [PARTITION_ID],
        |row| row.get(0),
    )
    .unwrap()
}

/// ORB-15534: a listing's freshness scan recorded its envelope stamps from
/// inside the task boundary, waiting out the registry's busy timeout behind
/// drain writers while every admission queued behind the read. The stamps
/// are a cache: a held write lock skips them, and the next scan records them.
#[test]
fn a_listing_skips_envelope_stamps_while_another_writer_holds_the_registry() {
    if !isolated(
        "task_index::a_listing_skips_envelope_stamps_while_another_writer_holds_the_registry",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let owner = Coordinated::open(root.path());
    let created = owner.create_task("Stamped while contended");
    let mut sql = rusqlite::Connection::open(task_registry_path(root.path())).unwrap();

    let writer = sql
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let started = Instant::now();
    let tasks = owner.backends.task.task.list_tasks().unwrap();
    let elapsed = started.elapsed();
    writer.rollback().unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, created.id);
    let busy_timeout =
        Duration::from_millis(orbit_common::storage::sqlite::DEFAULT_BUSY_TIMEOUT_MS.into());
    assert!(
        elapsed < busy_timeout / 2,
        "the listing must not wait for the registry write lock: took {elapsed:?}"
    );
    assert_eq!(
        recorded_stamps(&sql),
        0,
        "a held write lock skips the stamps"
    );

    assert_eq!(owner.backends.task.task.list_tasks().unwrap().len(), 1);
    assert_eq!(
        recorded_stamps(&sql),
        1,
        "the next scan records the stamp the contended one skipped"
    );
}

/// Counts `task.yaml` parses: the store opens one span per envelope read.
struct EnvelopeReads(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl<S: tracing::Subscriber> Layer<S> for EnvelopeReads {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: Context<'_, S>,
    ) {
        if attrs.metadata().name() == "task_envelope_read" {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// One selection on a fresh store handle, as a new process makes it: the
/// envelope reads it paid for and how long it took.
fn cold_selection(root: &Path, filter: &TaskListFilter) -> (usize, usize, Duration) {
    let owner = Coordinated::open(root);
    let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let subscriber = Registry::default().with(EnvelopeReads(std::sync::Arc::clone(&reads)));
    let (selected, elapsed) = tracing::subscriber::with_default(subscriber, || {
        let started = Instant::now();
        let selected = owner
            .backends
            .task
            .task
            .task_candidates(filter, usize::MAX)
            .unwrap()
            .items
            .len();
        (selected, started.elapsed())
    });
    (
        selected,
        reads.load(std::sync::atomic::Ordering::Relaxed),
        elapsed,
    )
}

/// Readiness and the backlog snapshot select every open task. On a
/// workspace whose history is mostly closed work, that selection must cost
/// reads of the open tasks, not of the whole history: the generated index
/// answers the status predicate, and the freshness scan accepts recorded
/// envelope stamps without parsing.
#[test]
#[allow(clippy::print_stderr)]
fn open_task_selection_reads_only_open_envelopes() {
    const CLOSED: usize = 2000;
    const OPEN: usize = 20;
    const CHILDREN: usize = 5;
    if !isolated("task_index::open_task_selection_reads_only_open_envelopes") {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let owner = Coordinated::open(root.path());
    let open = (0..OPEN)
        .map(|index| {
            owner.create_task_with_status(
                &format!("Open {index}"),
                &["src/lib.rs"],
                TaskStatus::Backlog,
                None,
            )
        })
        .collect::<Vec<_>>();
    for index in 0..CLOSED {
        // A few closed tasks are children of an open one: the subtree an
        // epic root's inheritance check walks.
        let parent = (index < CHILDREN).then(|| open[0].id.clone());
        owner.create_task_with_status(
            &format!("Closed {index}"),
            &["src/lib.rs"],
            TaskStatus::Done,
            parent,
        );
    }
    // One listing in any process records the envelope stamps every later
    // process accepts without parsing.
    assert_eq!(
        owner.backends.task.task.list_tasks().unwrap().len(),
        OPEN + CLOSED
    );
    drop(owner);

    let open_filter = TaskListFilter {
        statuses: Some(
            TaskStatus::ALL
                .into_iter()
                .filter(|status| !status.is_terminal())
                .collect(),
        ),
        ..TaskListFilter::default()
    };
    let (selected, reads, _) = cold_selection(root.path(), &open_filter);
    assert_eq!(selected, OPEN);
    assert!(
        (OPEN..=100).contains(&reads),
        "the open selection must read the open envelopes and at most 100: read {reads}"
    );
    let (children, child_reads, _) = cold_selection(
        root.path(),
        &TaskListFilter {
            parent_id: Some(open[0].id.clone()),
            ..TaskListFilter::default()
        },
    );
    assert_eq!(children, CHILDREN);
    assert!(
        child_reads <= CHILDREN,
        "a parent's children are selected from the relation index: read {child_reads}"
    );
    // The unfiltered listing readiness used to make reads every envelope:
    // a control that the counter sees each parse.
    let (all, baseline_reads, _) = cold_selection(root.path(), &TaskListFilter::default());
    assert_eq!(all, OPEN + CLOSED);
    assert!(baseline_reads >= OPEN + CLOSED, "read {baseline_reads}");

    // Timing evidence, not an assertion: the best of three cold runs each.
    let best = |filter: &TaskListFilter| {
        (0..3)
            .map(|_| cold_selection(root.path(), filter).2)
            .min()
            .unwrap()
    };
    let baseline = best(&TaskListFilter::default());
    let selection = best(&open_filter);
    eprintln!(
        "open-task selection over {CLOSED} closed + {OPEN} open tasks: \
         baseline (every task) {baseline:?}, open statuses {selection:?}, \
         {:.1}x faster",
        baseline.as_secs_f64() / selection.as_secs_f64().max(f64::EPSILON)
    );
}
