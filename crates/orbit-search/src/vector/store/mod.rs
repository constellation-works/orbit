//! `VectorStore` — the SQLite-backed orbit-search index.
//!
//! Module layout:
//!
//! - [`schema`] — DDL for `embeddings`, the `chunks` content table, and the
//!   `corpus_fts` FTS5 index built over it, plus the in-place migrations from
//!   earlier layouts.
//! - [`upsert`] — `upsert_embeddings`, the BLAKE3-deduped per-field write path,
//!   plus its private SQL helpers (`delete_field_rows`, content-hash check).
//! - [`tasks`] — `index_task` / `reindex_tasks` task-corpus entry points.
//! - [`docs`] — `index_doc` / `reindex_docs` docs-corpus entry points.
//! - [`queries`] — `delete_source` and `stats` read/cascade operations.
//!
//! This file owns the `VectorStore` struct itself plus the connection-handle
//! plumbing (`open`, `open_in_memory`, `connection` — pragma defaults come
//! from `orbit_common::storage::sqlite`) and the small `pub(super)`
//! constants shared across the submodules above.

mod docs;
mod queries;
pub(crate) mod schema;
mod tasks;
mod upsert;

use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use rusqlite::Connection;

pub const SOURCE_KIND_TASK: &str = "task";
// ADR-0180: docs share the embeddings table through source_kind, not a separate schema.
pub const SOURCE_KIND_DOC: &str = "doc";

#[derive(Clone)]
pub struct VectorStore {
    conn: Arc<Mutex<Connection>>,
}

impl VectorStore {
    /// Open the workspace-local orbit-search SQLite at `path`, applying the
    /// shared Orbit connection defaults (WAL best-effort, busy_timeout,
    /// foreign_keys, synchronous=NORMAL) and creating the
    /// embeddings/chunks/corpus_fts schema if missing.
    pub fn open(path: &Path) -> Result<Self, OrbitError> {
        let conn = orbit_common::storage::sqlite::open_private(path)?.connection;
        if let Err(error) = schema::ensure_vector_schema(&conn) {
            if !error.is_readonly_or_access_failure() {
                return Err(error);
            }
            // An index that already exists stays readable: the schema call only
            // wanted to re-apply or migrate DDL this process cannot write. One
            // that carries no index at all has nothing to read and nothing a
            // writer could ever land in, so the refusal is the whole answer
            // rather than an incidental one.
            if !schema::vector_schema_present(&conn)? {
                return Err(error);
            }
            tracing::warn!(
                target: "orbit.search.vector",
                path = %path.display(),
                error = %error,
                "skipped incidental semantic-index schema persistence"
            );
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Open an in-memory orbit-search database. Used by tests.
    pub fn open_in_memory() -> Result<Self, OrbitError> {
        let conn = Connection::open_in_memory().map_err(|e| OrbitError::Store(e.to_string()))?;
        orbit_common::storage::sqlite::apply_default_pragmas(&conn)?;
        schema::ensure_vector_schema(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub(super) fn connection(&self) -> Arc<Mutex<Connection>> {
        self.conn.clone()
    }
}

#[cfg(test)]
mod tests;
