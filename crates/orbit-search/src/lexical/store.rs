//! SQLite storage for task chunks and their FTS5 index.
use super::{
    SOURCE_KIND_TASK,
    chunker::chunk_text,
    migration,
    task_fields::{TaskSidecarSearchText, task_fields_with_sidecars},
};
use orbit_common::OrbitError;
use orbit_types::task::Task;
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

#[derive(Clone)]
pub struct LexicalStore {
    conn: Arc<Mutex<Connection>>,
    fts_queries: Arc<AtomicU64>,
    read_only: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct SearchIndexStats {
    pub chunks: usize,
    pub tasks: usize,
}
fn err(error: rusqlite::Error) -> OrbitError {
    orbit_common::storage::sqlite::sqlite_store_error(error)
}
impl LexicalStore {
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        let opened = orbit_common::storage::sqlite::open_private(path)?;
        let mut read_only = opened.read_only;
        let conn = opened.connection;
        if let Err(error) = migration::ensure_schema(&conn) {
            if !error.is_readonly_or_access_failure() || !migration::schema_present(&conn)? {
                return Err(error);
            }
            // The schema is already built, so reads proceed; a storage layer
            // that refused the migration write will refuse index writes too.
            read_only = true;
            tracing::warn!(%error, "search index migration requires writable storage");
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            fts_queries: Arc::new(AtomicU64::new(0)),
            read_only,
        })
    }
    pub fn open_read_only(path: &Path) -> Result<Self, OrbitError> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(err)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            fts_queries: Arc::new(AtomicU64::new(0)),
            read_only: true,
        })
    }
    pub fn open_in_memory() -> Result<Self, OrbitError> {
        let conn = Connection::open_in_memory().map_err(err)?;
        migration::ensure_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            fts_queries: Arc::new(AtomicU64::new(0)),
            read_only: false,
        })
    }
    /// Whether this connection can never write: it was opened read-only, or
    /// its storage refused writes at open (a read-only mount or sandbox).
    /// Callers that index best-effort on every task write check this instead
    /// of attempting a write that is known to fail. [ORB-24077]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
    pub(super) fn connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }
    /// Count one BM25 page query and emit it on `orbit.search.fts`, the event
    /// composed-runtime tests observe to prove a full page runs one query.
    pub(super) fn record_fts_query(&self) {
        let fts_queries = self.fts_queries.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::debug!(target: "orbit.search.fts", fts_queries, "BM25 page query");
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
        self.index_task_with_sidecars(task, &TaskSidecarSearchText::default(), false)
    }
    /// [`Self::index_task`] also indexing the task's comment bodies and
    /// artifact paths, and recording that coverage: search may then answer
    /// this task's substring supplement from the index alone instead of
    /// reading its bundle documents.
    pub fn index_task_with_sidecar_text(
        &self,
        task: &Task,
        sidecars: &TaskSidecarSearchText,
    ) -> Result<usize, OrbitError> {
        self.index_task_with_sidecars(task, sidecars, true)
    }
    fn index_task_with_sidecars(
        &self,
        task: &Task,
        sidecars: &TaskSidecarSearchText,
        sidecar_coverage: bool,
    ) -> Result<usize, OrbitError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(err)?;
        tx.execute(
            "DELETE FROM chunks WHERE source_kind = ?1 AND source_id = ?2",
            params![SOURCE_KIND_TASK, task.id],
        )
        .map_err(err)?;
        let count = insert_task(&tx, task, sidecars)?;
        record_coverage(
            &tx,
            &task.id,
            if sidecar_coverage {
                FIELDS_VERSION_SIDECARS
            } else {
                FIELDS_VERSION_TASK_ONLY
            },
        )?;
        tx.commit().map_err(err)?;
        Ok(count)
    }
    /// Replace the entire task corpus atomically, including stale sources.
    pub fn reindex_tasks(&self, tasks: &[Task]) -> Result<SearchIndexStats, OrbitError> {
        let records = tasks
            .iter()
            .map(|task| (task, TaskSidecarSearchText::default(), false))
            .collect::<Vec<_>>();
        self.reindex_task_records(&records)
    }
    /// [`Self::reindex_tasks`] with each task's sidecar text where the
    /// caller could read it: `Some` records sidecar coverage for that
    /// task, `None` rebuilds it from its task fields at the lesser
    /// coverage, all in the one rebuild transaction.
    pub fn reindex_tasks_with_sidecar_text(
        &self,
        tasks: &[(Task, Option<TaskSidecarSearchText>)],
    ) -> Result<SearchIndexStats, OrbitError> {
        let records = tasks
            .iter()
            .map(|(task, sidecars)| {
                (
                    task,
                    sidecars.clone().unwrap_or_default(),
                    sidecars.is_some(),
                )
            })
            .collect::<Vec<_>>();
        self.reindex_task_records(&records)
    }
    fn reindex_task_records(
        &self,
        records: &[(&Task, TaskSidecarSearchText, bool)],
    ) -> Result<SearchIndexStats, OrbitError> {
        let mut conn = self.lock()?;
        let tx = conn.transaction().map_err(err)?;
        tx.execute(
            "DELETE FROM chunks WHERE source_kind = ?1",
            [SOURCE_KIND_TASK],
        )
        .map_err(err)?;
        if migration::table_exists(&tx, "source_index_coverage")? {
            tx.execute(
                "DELETE FROM source_index_coverage WHERE source_kind = ?1",
                [SOURCE_KIND_TASK],
            )
            .map_err(err)?;
        }
        let mut chunks = 0;
        for (task, sidecars, sidecar_coverage) in records {
            chunks += insert_task(&tx, task, sidecars)?;
            record_coverage(
                &tx,
                &task.id,
                if *sidecar_coverage {
                    FIELDS_VERSION_SIDECARS
                } else {
                    FIELDS_VERSION_TASK_ONLY
                },
            )?;
        }
        tx.commit().map_err(err)?;
        Ok(SearchIndexStats {
            chunks,
            tasks: records.len(),
        })
    }
    /// The task ids whose chunks carry sidecar coverage (comment bodies
    /// and artifact paths): the tasks a substring search may answer from
    /// this index without reading bundle documents. Tasks indexed before
    /// coverage was recorded, or only ever through [`Self::index_task`], are
    /// absent until a sidecar-aware reindex or task write covers them.
    pub fn sidecar_indexed_task_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.lock()?;
        if !migration::table_exists(&conn, "source_index_coverage")? {
            return Ok(BTreeSet::new());
        }
        let mut stmt = conn
            .prepare(
                "SELECT source_id FROM source_index_coverage \
                 WHERE source_kind = ?1 AND fields_version >= ?2",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map(params![SOURCE_KIND_TASK, FIELDS_VERSION_SIDECARS], |row| {
                row.get::<_, String>(0)
            })
            .map_err(err)?;
        let mut ids = BTreeSet::new();
        for row in rows {
            ids.insert(row.map_err(err)?);
        }
        Ok(ids)
    }
    /// The task ids with a chunk containing `query` as a substring, with the
    /// bundle matcher's case-insensitive, diacritics-intact semantics.
    /// Queries of three or more characters go through the trigram index as
    /// one phrase; shorter ones (which form no trigram) and stores opened
    /// read-only before the trigram table existed fall back to an `instr`
    /// scan over chunk text — still no bundle reads either way.
    pub fn substring_task_ids(&self, query: &str) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.lock()?;
        let use_trigram =
            query.chars().count() >= 3 && migration::table_exists(&conn, "corpus_substring_fts")?;
        let (sql, needle) = if use_trigram {
            (
                "SELECT DISTINCT chunks.source_id FROM corpus_substring_fts \
                 JOIN chunks ON chunks.id = corpus_substring_fts.rowid \
                 WHERE corpus_substring_fts MATCH ?1 AND chunks.source_kind = ?2",
                format!("\"{}\"", query.replace('"', "\"\"")),
            )
        } else {
            (
                "SELECT DISTINCT source_id FROM chunks \
                 WHERE source_kind = ?2 AND instr(lower(content), ?1) > 0",
                query.to_lowercase(),
            )
        };
        let mut stmt = conn.prepare(sql).map_err(err)?;
        let rows = stmt
            .query_map(params![needle, SOURCE_KIND_TASK], |row| {
                row.get::<_, String>(0)
            })
            .map_err(err)?;
        let mut ids = BTreeSet::new();
        for row in rows {
            ids.insert(row.map_err(err)?);
        }
        Ok(ids)
    }
    pub fn delete_source(&self, kind: &str, id: &str) -> Result<(), OrbitError> {
        let conn = self.lock()?;
        conn.execute(
            "DELETE FROM chunks WHERE source_kind = ?1 AND source_id = ?2",
            params![kind, id],
        )
        .map_err(err)?;
        if migration::table_exists(&conn, "source_index_coverage")? {
            conn.execute(
                "DELETE FROM source_index_coverage WHERE source_kind = ?1 AND source_id = ?2",
                params![kind, id],
            )
            .map_err(err)?;
        }
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
/// Coverage of a task indexed from its [`Task`] fields alone.
const FIELDS_VERSION_TASK_ONLY: i64 = 1;
/// Coverage of a task also indexed with its comment bodies and artifact
/// paths: every field the bundle matcher reads is in `chunks`.
const FIELDS_VERSION_SIDECARS: i64 = 2;

fn record_coverage(
    conn: &Connection,
    task_id: &str,
    fields_version: i64,
) -> Result<(), OrbitError> {
    if !migration::table_exists(conn, "source_index_coverage")? {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO source_index_coverage(source_kind, source_id, fields_version) \
         VALUES (?1, ?2, ?3) \
         ON CONFLICT(source_kind, source_id) DO UPDATE SET fields_version = excluded.fields_version",
        params![SOURCE_KIND_TASK, task_id, fields_version],
    )
    .map_err(err)?;
    Ok(())
}

fn insert_task(
    conn: &Connection,
    task: &Task,
    sidecars: &TaskSidecarSearchText,
) -> Result<usize, OrbitError> {
    let mut count = 0;
    let mut stmt = conn.prepare_cached("INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content) VALUES (?1, ?2, ?3, ?4, ?5)").map_err(err)?;
    for field in task_fields_with_sidecars(task, sidecars) {
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
