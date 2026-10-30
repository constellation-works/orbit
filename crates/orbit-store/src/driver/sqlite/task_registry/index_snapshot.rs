//! Reuse the registry half of task freshness scans until SQLite changes.
//!
//! A dedicated query-only connection gives `data_version` a stable identity:
//! SQLite versions are comparable only on the same connection. All registry
//! writes, including this handle's writer, use other connections and invalidate
//! it. This never caches filesystem freshness; every scan still probes every
//! registered envelope and reports strict read failures as before.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::{apply_default_pragmas, sqlite_store_error};
use rusqlite::{Connection, OpenFlags};

use super::store::TaskRegistryStore;
use crate::contracts::{EnvelopeStampRecord, IndexedTaskRow, TaskBundleBinding};

pub(crate) struct TaskIndexSnapshot {
    pub(crate) registered: Vec<TaskBundleBinding>,
    pub(crate) indexed_count: usize,
    /// Registry rows aligned with the bindings; absent for an incomplete index.
    pub(crate) rows: Option<Vec<Arc<TaskIndexProofRow>>>,
}

/// One immutable index row and its proof, from the same registry refresh.
pub(crate) struct TaskIndexProofRow {
    pub(crate) task_id: String,
    pub(crate) indexed: IndexedTaskRow,
    pub(crate) fingerprint: String,
    pub(crate) recorded: Option<EnvelopeStampRecord>,
}

fn load_task_index_snapshot(
    registry: &TaskRegistryStore,
    partition_id: &str,
) -> Result<Arc<TaskIndexSnapshot>, OrbitError> {
    let registered = registry.tasks_for_workspace(partition_id)?;
    let mut indexed = registry.indexed_task_rows_for_workspace(partition_id)?;
    let indexed_count = indexed.len();
    // As in an uncached scan, a row-count mismatch goes straight to fallback
    // without consulting persisted proofs.
    let rows = if registered.len() == indexed_count {
        let mut recorded = registry.envelope_stamps_for_workspace(partition_id)?;
        registered
            .iter()
            .map(|binding| {
                let row = indexed.remove(&binding.task_id)?;
                Some(Arc::new(TaskIndexProofRow {
                    task_id: binding.task_id.clone(),
                    fingerprint: row.fingerprint(),
                    indexed: row,
                    recorded: recorded.remove(&binding.task_id),
                }))
            })
            .collect()
    } else {
        None
    };
    Ok(Arc::new(TaskIndexSnapshot {
        registered,
        indexed_count,
        rows,
    }))
}

struct SnapshotReader {
    connection: Connection,
    cached: Option<(String, i64, Arc<TaskIndexSnapshot>)>,
}

/// One retained partition snapshot per registry handle, shared by its clones.
pub(super) struct TaskIndexSnapshots {
    path: PathBuf,
    reader: Mutex<Option<SnapshotReader>>,
}

impl TaskIndexSnapshots {
    pub(super) fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            reader: Mutex::new(None),
        }
    }

    pub(super) fn snapshot_for(
        &self,
        registry: &TaskRegistryStore,
        partition_id: &str,
    ) -> Result<Arc<TaskIndexSnapshot>, OrbitError> {
        let mut reader = orbit_common::fs::lock_wait::mutex(&self.reader)
            .map_err(|error| OrbitError::Store(format!("task snapshot mutex poisoned: {error}")))?;
        if reader.is_none() {
            let connection =
                Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(sqlite_store_error)?;
            apply_default_pragmas(&connection)?;
            *reader = Some(SnapshotReader {
                connection,
                cached: None,
            });
        }
        let Some(reader) = reader.as_mut() else {
            unreachable!("snapshot reader initialized above");
        };
        // Sample BEFORE loading rows. A commit during the load leaves this
        // cache labelled with the old version, forcing a refresh next time.
        // Never label older rows with a version sampled after their read.
        let version = reader
            .connection
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_store_error)?;
        if let Some((partition, cached_version, snapshot)) = &reader.cached
            && partition == partition_id
            && *cached_version == version
        {
            return Ok(Arc::clone(snapshot));
        }
        let snapshot = load_task_index_snapshot(registry, partition_id)?;
        reader.cached = Some((partition_id.to_string(), version, Arc::clone(&snapshot)));
        Ok(snapshot)
    }
}

impl TaskRegistryStore {
    pub(crate) fn task_index_snapshot(
        &self,
        partition_id: &str,
    ) -> Result<Arc<TaskIndexSnapshot>, OrbitError> {
        // Read-only admission may have fallen back to an immutable main-file
        // connection on a mount without writable WAL sidecars. Preserve that
        // connection's read semantics instead of requiring another live one.
        if self.is_read_only()? {
            return load_task_index_snapshot(self, partition_id);
        }
        self.index_snapshots.snapshot_for(self, partition_id)
    }
}
