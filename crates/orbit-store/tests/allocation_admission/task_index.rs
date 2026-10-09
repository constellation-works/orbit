//! Generated-index observation and publication through composed task backends.

use std::sync::Mutex;

use orbit_store::compose::{WorkspaceTaskBackends, workspace_observational_backends};
use orbit_store::contracts::TaskDocumentUpdateParams;
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
