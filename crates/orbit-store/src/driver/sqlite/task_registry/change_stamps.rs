//! Reuse task change stamps until a registry commit.
//!
//! A dedicated query-only connection gives `data_version` a stable identity:
//! SQLite versions are comparable only on the same connection. Registry writes
//! use other connections and invalidate this cache. The version is sampled
//! before the row load, so a commit during the load leaves the cache labelled
//! with the old version and the next read refreshes it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::{apply_default_pragmas, sqlite_store_error};
use rusqlite::{Connection, OpenFlags};

use crate::contracts::TaskChangeStamp;

/// The index read a change watcher compares between scans.
const TASK_CHANGE_STAMP_SQL: &str =
    "SELECT task_id, status, updated_at FROM task_bundle_index WHERE workspace_id = ?1";

struct StampReader {
    connection: Connection,
    cached: Option<(String, i64, Arc<Vec<TaskChangeStamp>>)>,
}

/// One retained partition stamp list per registry handle, shared by its clones.
pub(super) struct TaskChangeStamps {
    path: PathBuf,
    reader: Mutex<Option<StampReader>>,
}

impl TaskChangeStamps {
    pub(super) fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            reader: Mutex::new(None),
        }
    }

    /// Stamps for `partition_id`, or the snapshot from the last read when the
    /// registry has not committed since. The version is sampled before the
    /// row load.
    pub(super) fn change_stamps_for(
        &self,
        partition_id: &str,
    ) -> Result<Arc<Vec<TaskChangeStamp>>, OrbitError> {
        let mut reader = orbit_common::fs::lock_wait::mutex(&self.reader).map_err(|error| {
            OrbitError::Store(format!("task change stamp mutex poisoned: {error}"))
        })?;
        if reader.is_none() {
            let connection =
                Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                    .map_err(sqlite_store_error)?;
            apply_default_pragmas(&connection)?;
            *reader = Some(StampReader {
                connection,
                cached: None,
            });
        }
        let Some(reader) = reader.as_mut() else {
            return Err(OrbitError::Store(
                "task change stamp reader missing after open".into(),
            ));
        };
        // Sample BEFORE loading rows. A commit during the load leaves this
        // cache labelled with the old version, forcing a refresh next time.
        // Never label older rows with a version sampled after their read.
        let version = reader
            .connection
            .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
            .map_err(sqlite_store_error)?;
        if let Some((partition, cached_version, stamps)) = &reader.cached
            && partition == partition_id
            && *cached_version == version
        {
            orbit_common::tracing::debug!(
                target: "orbit.store.task_query",
                partition_id = %partition_id,
                path = "cache",
                index_rows = 0,
                "task change stamps unchanged",
            );
            return Ok(Arc::clone(stamps));
        }
        let stamps = Arc::new(read_task_change_stamps(&reader.connection, partition_id)?);
        reader.cached = Some((partition_id.to_string(), version, Arc::clone(&stamps)));
        Ok(stamps)
    }
}

/// Read one partition's stamps from `conn`. `partition_id` is already validated.
pub(super) fn read_task_change_stamps(
    conn: &Connection,
    partition_id: &str,
) -> Result<Vec<TaskChangeStamp>, OrbitError> {
    let mut stmt = conn
        .prepare_cached(TASK_CHANGE_STAMP_SQL)
        .map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map([partition_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(sqlite_store_error)?;
    let mut stamps = Vec::new();
    for row in rows {
        let (id, raw_status, raw_updated_at) = row.map_err(sqlite_store_error)?;
        let status = super::listing::parse_indexed_status(&id, &raw_status)?;
        let updated_at = chrono::DateTime::parse_from_rfc3339(&raw_updated_at)
            .map_err(|error| {
                OrbitError::Store(format!(
                    "invalid indexed updated_at '{raw_updated_at}' for task '{id}': {error}"
                ))
            })?
            .with_timezone(&chrono::Utc);
        stamps.push(TaskChangeStamp {
            id,
            status,
            updated_at,
        });
    }
    orbit_common::tracing::debug!(
        target: "orbit.store.task_query",
        partition_id = %partition_id,
        path = "index",
        index_rows = stamps.len(),
        "task change stamps loaded",
    );
    Ok(stamps)
}
