//! Registry reads run on pooled read-only connections, so a long write
//! transaction (`replace_task_index` over a whole workspace) no longer
//! stalls every concurrent list/show that shares the registry file.

use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use rusqlite::TransactionBehavior;
use tempfile::TempDir;

use super::{bind, envelope, registry_path, store};
use crate::driver::sqlite::task_registry::TaskRegistryStore;

/// Register one canonical bundle and project it into the task index.
fn seed_indexed_task(store: &TaskRegistryStore, partition_id: &str, status: TaskStatus) -> String {
    let task_id = store
        .allocate_task_id(partition_id)
        .expect("allocate task id");
    let path = store
        .canonical_task_bundle_path(partition_id, &task_id)
        .expect("canonical bundle path");
    fs::create_dir_all(&path).expect("create canonical bundle");
    store
        .register_task_bundle(&task_id, partition_id, &path)
        .expect("register canonical bundle");
    store
        .replace_task_index(
            partition_id,
            &envelope(&task_id, status, Vec::new(), Vec::new()),
        )
        .expect("index task");
    task_id
}

#[test]
fn pooled_registry_reader_is_query_only() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);

    let conn = store.read().expect("read connection");
    let result = conn.execute(
        "UPDATE allocator_state SET next_number = 99 WHERE authority = 'local'",
        [],
    );
    assert!(
        result.is_err(),
        "a write through a pooled registry reader must fail (query_only=ON)"
    );
}

#[test]
fn registry_readers_are_returned_to_the_pool_and_reused() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);

    drop(store.read().expect("first read"));
    drop(store.read().expect("second read"));
    let pool = store
        .reader_pool_for_test()
        .expect("writable registry has a read pool");
    assert_eq!(pool.idle_len(), 1, "sequential reads reuse one connection");

    let a = store.read().expect("read a");
    let b = store.read().expect("read b");
    assert_eq!(pool.idle_len(), 0);
    drop(a);
    drop(b);
    assert_eq!(pool.idle_len(), 2, "both readers checked back in");
}

#[test]
fn registry_reads_see_writes_committed_by_the_writer_connection() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());

    let task_id = seed_indexed_task(&store, &workspace.partition_id, TaskStatus::Backlog);

    let statuses = store.global_task_status_index().expect("status projection");
    assert_eq!(statuses.get(&task_id), Some(&TaskStatus::Backlog));
    assert_eq!(
        store
            .tasks_for_workspace(&workspace.partition_id)
            .expect("task bindings")
            .len(),
        1
    );
}

/// The core guarantee: a registry read completes while the writer holds an
/// open IMMEDIATE transaction. Routed through the writer mutex (the shape
/// before this change) the read would block until the transaction ended.
#[test]
fn registry_reads_do_not_queue_behind_an_open_writer_transaction() {
    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let seeded = seed_indexed_task(&store, &workspace.partition_id, TaskStatus::Backlog);

    let (tx_open_send, tx_open_recv) = mpsc::channel::<()>();
    let (read_done_send, read_done_recv) = mpsc::channel::<usize>();

    let writer_store = store.clone();
    let writer_task = seeded.clone();
    let writer = thread::spawn(move || -> Result<(), OrbitError> {
        let mut conn = writer_store
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx.execute(
            "UPDATE task_bundle_index SET status = 'in-progress' WHERE task_id = ?1",
            [&writer_task],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        tx_open_send
            .send(())
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        // Hold the write transaction open until the reader finishes, or prove
        // the reader queued behind it by timing out.
        read_done_recv
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| {
                OrbitError::Store("reader did not complete while writer tx open".to_string())
            })?;
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    });

    tx_open_recv
        .recv_timeout(Duration::from_secs(10))
        .expect("writer opened its transaction");

    let statuses = store
        .global_task_status_index()
        .expect("read while writer tx open");
    let bindings = store
        .tasks_for_workspace(&workspace.partition_id)
        .expect("read while writer tx open");
    read_done_send.send(bindings.len()).expect("signal reader");

    writer
        .join()
        .expect("writer thread")
        .expect("writer transaction commits");

    assert_eq!(
        statuses.get(&seeded),
        Some(&TaskStatus::Backlog),
        "the reader sees the committed snapshot, not the open transaction"
    );
    assert_eq!(bindings.len(), 1);
    assert_eq!(
        store
            .global_task_status_index()
            .expect("status projection after commit")
            .get(&seeded),
        Some(&TaskStatus::InProgress),
        "the committed write is visible to a later read"
    );
}

/// Parallel readers against a committing writer: no `database is locked`
/// surfaces with WAL plus pooled readers.
#[test]
fn concurrent_registry_readers_and_writer_produce_no_locked_errors() {
    const READER_THREADS: usize = 4;
    const READS_PER_THREAD: usize = 25;
    const WRITES: usize = 25;

    let temp = TempDir::new().expect("tempdir");
    let store = store(&temp);
    let workspace = bind(&store, temp.path());
    let seeded = seed_indexed_task(&store, &workspace.partition_id, TaskStatus::Backlog);

    let mut readers = Vec::new();
    for _ in 0..READER_THREADS {
        let store = store.clone();
        let partition_id = workspace.partition_id.clone();
        readers.push(thread::spawn(move || -> Result<(), OrbitError> {
            for _ in 0..READS_PER_THREAD {
                store.global_task_status_index()?;
                store.tasks_for_workspace(&partition_id)?;
                store.local_task_prefix()?;
            }
            Ok(())
        }));
    }

    for i in 0..WRITES {
        let status = if i % 2 == 0 {
            TaskStatus::InProgress
        } else {
            TaskStatus::Backlog
        };
        store
            .replace_task_index(
                &workspace.partition_id,
                &envelope(&seeded, status, Vec::new(), Vec::new()),
            )
            .expect("concurrent index write");
    }

    for reader in readers {
        reader
            .join()
            .expect("reader thread")
            .expect("reads succeed alongside writes");
    }
}

#[test]
fn read_only_registry_reads_fall_back_to_the_writer_connection() {
    let temp = TempDir::new().expect("tempdir");
    let path = registry_path(&temp);
    {
        let store = store(&temp);
        let workspace = bind(&store, temp.path());
        seed_indexed_task(&store, &workspace.partition_id, TaskStatus::Done);
    }

    let mut permissions = fs::metadata(&path)
        .expect("registry metadata")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).expect("mark registry read-only");

    let observer = TaskRegistryStore::open(&path).expect("open read-only registry");
    assert!(
        observer.reader_pool_for_test().is_none(),
        "a read-only registry has no read pool"
    );
    assert_eq!(
        observer
            .global_task_status_index()
            .expect("read through the writer connection")
            .len(),
        1
    );
}
