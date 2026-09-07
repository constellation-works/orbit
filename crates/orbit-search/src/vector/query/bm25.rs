use orbit_common::OrbitError;
use rusqlite::params;

use crate::vector::store::VectorStore;

#[derive(Debug, Clone, PartialEq)]
pub struct Bm25Hit {
    pub source_kind: String,
    pub source_id: String,
    pub field: String,
    /// `chunks.id`, which is also the matching `corpus_fts` rowid — the direct
    /// address a snippet lookup uses instead of re-deriving the chunk.
    pub rowid: i64,
    pub rank: usize,
}

pub fn bm25_top_k(
    store: &VectorStore,
    query: &str,
    kind: Option<&str>,
    limit: usize,
) -> Result<Vec<Bm25Hit>, OrbitError> {
    if query.trim().is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let match_query = fts_terms_query(query);
    let conn = store.connection();
    let conn = conn
        .lock()
        .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
    let mut hits = Vec::new();
    if let Some(kind) = kind {
        let mut stmt = conn
            .prepare(
                r#"
                    SELECT
                        chunks.source_kind,
                        chunks.source_id,
                        chunks.field,
                        chunks.id,
                        bm25(corpus_fts) AS rank
                    FROM corpus_fts
                    JOIN chunks ON chunks.id = corpus_fts.rowid
                    WHERE corpus_fts MATCH ?1 AND chunks.source_kind = ?2
                    ORDER BY rank
                    LIMIT ?3
                "#,
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut rows = stmt
            .query(params![match_query, kind, limit as i64])
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        collect_hits(&mut rows, &mut hits)?;
    } else {
        let mut stmt = conn
            .prepare(
                r#"
                    SELECT
                        chunks.source_kind,
                        chunks.source_id,
                        chunks.field,
                        chunks.id,
                        bm25(corpus_fts) AS rank
                    FROM corpus_fts
                    JOIN chunks ON chunks.id = corpus_fts.rowid
                    WHERE corpus_fts MATCH ?1
                    ORDER BY rank
                    LIMIT ?2
                "#,
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut rows = stmt
            .query(params![match_query, limit as i64])
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        collect_hits(&mut rows, &mut hits)?;
    }
    Ok(hits)
}

fn collect_hits(rows: &mut rusqlite::Rows<'_>, hits: &mut Vec<Bm25Hit>) -> Result<(), OrbitError> {
    while let Some(row) = rows
        .next()
        .map_err(|error| OrbitError::Store(error.to_string()))?
    {
        hits.push(Bm25Hit {
            source_kind: row
                .get(0)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
            source_id: row
                .get(1)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
            field: row
                .get(2)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
            rowid: row
                .get(3)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
            rank: hits.len() + 1,
        });
    }
    Ok(())
}

pub fn snippet_for_hit(
    store: &VectorStore,
    source_kind: &str,
    source_id: &str,
    field: &str,
    chunk_idx: Option<usize>,
    rowid: Option<i64>,
) -> Result<Option<String>, OrbitError> {
    if let Some(rowid) = rowid {
        return snippet_by_rowid(store, rowid);
    }
    let Some(chunk_idx) = chunk_idx else {
        return Ok(None);
    };
    snippet_by_chunk_idx(store, source_kind, source_id, field, chunk_idx)
}

fn snippet_by_rowid(store: &VectorStore, rowid: i64) -> Result<Option<String>, OrbitError> {
    let conn = store.connection();
    let conn = conn
        .lock()
        .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
    conn.query_row(
        "SELECT content FROM chunks WHERE id = ?1",
        params![rowid],
        |row| row.get::<_, String>(0),
    )
    .map(Some)
    .or_else(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(OrbitError::Store(other.to_string())),
    })
}

fn snippet_by_chunk_idx(
    store: &VectorStore,
    source_kind: &str,
    source_id: &str,
    field: &str,
    chunk_idx: usize,
) -> Result<Option<String>, OrbitError> {
    let conn = store.connection();
    let conn = conn
        .lock()
        .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
    conn.query_row(
        r#"
            SELECT content
            FROM chunks
            WHERE source_kind = ?1 AND source_id = ?2 AND field = ?3 AND chunk_idx = ?4
        "#,
        params![source_kind, source_id, field, chunk_idx as i64],
        |row| row.get::<_, String>(0),
    )
    .map(Some)
    .or_else(|error| match error {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(OrbitError::Store(other.to_string())),
    })
}

/// Turn a free-text query into an FTS5 `MATCH` expression: every
/// whitespace-separated term is quoted (so `-`, `*`, `NOT`, and the like are
/// literal), and the terms are joined with FTS5's implicit AND. Quoting the
/// whole query as one string would make it a *phrase* query, which only hits
/// chunks where the words are adjacent — a multi-word search returned nothing
/// from the lexical half of hybrid ranking.
// widened for tests per ORB-00230 sibling layout; see test_layout.md
pub(crate) fn fts_terms_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}
