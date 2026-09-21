//! SQLite storage for task chunks and their FTS5 index.
use super::{SOURCE_KIND_TASK, chunker::chunk_text, migration, task_fields::task_fields};
use orbit_common::OrbitError;
use orbit_types::task::Task;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct LexicalStore {
    conn: Arc<Mutex<Connection>>,
}
#[derive(Debug, Clone, Serialize)]
pub struct SearchIndexStats {
    pub chunks: usize,
    pub tasks: usize,
}
fn err(error: rusqlite::Error) -> OrbitError {
    OrbitError::Store(error.to_string())
}
impl LexicalStore {
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        let conn = orbit_common::storage::sqlite::open_private(path)?.connection;
        if let Err(error) = migration::ensure_schema(&conn) {
            if !error.is_readonly_or_access_failure() || !migration::schema_present(&conn)? {
                return Err(error);
            }
            tracing::warn!(%error, "search index migration requires writable storage");
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
    pub fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(err)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
    pub fn open_in_memory() -> Result<Self, OrbitError> {
        let conn = Connection::open_in_memory().map_err(err)?;
        migration::ensure_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }
    pub(super) fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
    fn lock(&self) -> Result<MutexGuard<'_, Connection>, OrbitError> {
        self.conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))
    }
    pub fn has_source_kind(&self, kind: &str) -> Result<bool, OrbitError> {
        self.lock()?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM chunks WHERE source_kind = ?1)",
                [kind],
                |row| row.get(0),
            )
            .map_err(err)
    }
    pub fn index_task(&self, task: &Task) -> Result<usize, OrbitError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(err)?;
        tx.execute(
            "DELETE FROM chunks WHERE source_kind = ?1 AND source_id = ?2",
            params![SOURCE_KIND_TASK, task.id],
        )
        .map_err(err)?;
        let count = insert_task(&tx, task)?;
        tx.commit().map_err(err)?;
        Ok(count)
    }
    /// Replace the entire task corpus atomically, including stale sources.
    pub fn reindex_tasks(&self, tasks: &[Task]) -> Result<SearchIndexStats, OrbitError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(err)?;
        tx.execute(
            "DELETE FROM chunks WHERE source_kind = ?1",
            [SOURCE_KIND_TASK],
        )
        .map_err(err)?;
        let mut chunks = 0;
        for task in tasks {
            chunks += insert_task(&tx, task)?;
        }
        tx.commit().map_err(err)?;
        Ok(SearchIndexStats {
            chunks,
            tasks: tasks.len(),
        })
    }
    pub fn delete_source(&self, kind: &str, id: &str) -> Result<(), OrbitError> {
        self.lock()?
            .execute(
                "DELETE FROM chunks WHERE source_kind = ?1 AND source_id = ?2",
                params![kind, id],
            )
            .map_err(err)?;
        Ok(())
    }
    pub fn stats(&self) -> Result<SearchIndexStats, OrbitError> {
        self.lock()?
            .query_row(
                "SELECT COUNT(*), COUNT(DISTINCT source_id) FROM chunks WHERE source_kind = 'task'",
                [],
                |row| {
                    Ok(SearchIndexStats {
                        chunks: row.get(0)?,
                        tasks: row.get(1)?,
                    })
                },
            )
            .map_err(err)
    }
}
fn insert_task(conn: &Connection, task: &Task) -> Result<usize, OrbitError> {
    let mut count = 0;
    let mut stmt = conn.prepare_cached("INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content) VALUES (?1, ?2, ?3, ?4, ?5)").map_err(err)?;
    for field in task_fields(task) {
        for (idx, chunk) in chunk_text(&field.text).iter().enumerate() {
            stmt.execute(params![
                SOURCE_KIND_TASK,
                task.id,
                field.field,
                idx as i64,
                chunk
            ])
            .map_err(err)?;
            count += 1;
        }
    }
    Ok(count)
}
