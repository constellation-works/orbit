//! Read/cascade operations over the index.
//!
//! `model_ids` reports the distinct embedding models present in the index.
//! `has_sources` answers whether a `source_kind` has any chunk rows at all.
//! `delete_source` cascades both the vector rows and the chunk rows (and with
//! them, through the `corpus_fts` triggers, the FTS5 index) for a given
//! `(source_kind, source_id)`. `stats` aggregates row counts by
//! `(source_kind, model_id)` and counts orphaned `task` rows whose
//! `source_id` is no longer in the live task corpus.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use rusqlite::params;

use super::VectorStore;
use crate::vector::{SemanticStats, SourceModelCount};

impl VectorStore {
    /// Whether the lexical corpus contains at least one row of `source_kind`.
    /// This is deliberately an indexed `EXISTS` probe rather than a source-ID
    /// listing: interactive search only needs to choose its retrieval path.
    pub fn has_source_kind(&self, source_kind: &str) -> Result<bool, OrbitError> {
        let conn = self.connection();
        let conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM chunks WHERE source_kind = ?1 LIMIT 1)",
            params![source_kind],
            |row| row.get(0),
        )
        .map_err(|error| OrbitError::Store(error.to_string()))
    }

    /// Whether any source of `source_kind` has chunk rows — and with them
    /// `corpus_fts` entries — in this index.
    ///
    /// One indexed lookup on `chunks_by_address`, so a caller can decide
    /// whether the index can answer a lexical query before it walks a corpus
    /// on disk instead [DANI-10369].
    pub fn has_sources(&self, source_kind: &str) -> Result<bool, OrbitError> {
        let conn = self.connection();
        let conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM chunks WHERE source_kind = ?1)",
            params![source_kind],
            |row| row.get::<_, i64>(0),
        )
        .map(|exists| exists != 0)
        .map_err(|error| super::schema::translate_corpus_fts_sql_error(&conn, error))
    }

    pub fn source_ids(&self, source_kind: &str) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.connection();
        let conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        let mut stmt = conn
            .prepare("SELECT DISTINCT source_id FROM embeddings WHERE source_kind = ?1")
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map(params![source_kind], |row| row.get::<_, String>(0))
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut source_ids = BTreeSet::new();
        for row in rows {
            source_ids.insert(row.map_err(|error| OrbitError::Store(error.to_string()))?);
        }
        Ok(source_ids)
    }

    /// Distinct embedding `model_id` values present in this index.
    ///
    /// Cosine scores are only comparable within one model, so a federated read
    /// consults this before claiming a workspace's vectors participate in a
    /// fused ranking [ORB-11027].
    pub fn model_ids(&self) -> Result<BTreeSet<String>, OrbitError> {
        let conn = self.connection();
        let conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        let mut stmt = conn
            .prepare("SELECT DISTINCT model_id FROM embeddings")
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut model_ids = BTreeSet::new();
        for row in rows {
            model_ids.insert(row.map_err(|error| OrbitError::Store(error.to_string()))?);
        }
        Ok(model_ids)
    }

    pub fn delete_source(&self, source_kind: &str, source_id: &str) -> Result<(), OrbitError> {
        let conn = self.connection();
        let conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        conn.execute(
            "DELETE FROM embeddings WHERE source_kind = ?1 AND source_id = ?2",
            params![source_kind, source_id],
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
        conn.execute(
            "DELETE FROM chunks WHERE source_kind = ?1 AND source_id = ?2",
            params![source_kind, source_id],
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
        Ok(())
    }

    pub fn stats(&self, current_task_ids: &[String]) -> Result<SemanticStats, OrbitError> {
        let conn = self.connection();
        let conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        let mut stmt = conn
            .prepare(
                r#"
                    SELECT source_kind, model_id, COUNT(*)
                    FROM embeddings
                    GROUP BY source_kind, model_id
                    ORDER BY source_kind, model_id
                "#,
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(SourceModelCount {
                    source_kind: row.get(0)?,
                    model_id: row.get(1)?,
                    rows: row.get::<_, i64>(2)? as usize,
                })
            })
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut counts = Vec::new();
        for row in rows {
            counts.push(row.map_err(|error| OrbitError::Store(error.to_string()))?);
        }

        let current = current_task_ids.iter().cloned().collect::<BTreeSet<_>>();
        let mut stmt = conn
            .prepare("SELECT DISTINCT source_id FROM embeddings WHERE source_kind = 'task'")
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut stale_rows = 0;
        for row in rows {
            let source_id = row.map_err(|error| OrbitError::Store(error.to_string()))?;
            if !current.contains(&source_id) {
                stale_rows += 1;
            }
        }
        Ok(SemanticStats { counts, stale_rows })
    }
}
