use orbit_common::OrbitError;
use rusqlite::params;

use crate::lexical::store::LexicalStore;

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
    store: &LexicalStore,
    query: &str,
    kind: Option<&str>,
    field: Option<&str>,
    limit: usize,
) -> Result<Vec<Bm25Hit>, OrbitError> {
    bm25_page(store, query, kind, field, 0, limit)
}

/// One page of the BM25 ranking: at most `limit` hits after skipping the
/// first `offset`. Ties on rank break by chunk id, so consecutive pages over
/// an unchanged index concatenate to exactly the `bm25_top_k` order, and
/// `rank` stays the absolute position in that order.
pub fn bm25_page(
    store: &LexicalStore,
    query: &str,
    kind: Option<&str>,
    field: Option<&str>,
    offset: usize,
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
    let fts_err = |error| crate::lexical::migration::translate_corpus_fts_sql_error(&conn, error);
    let mut hits = Vec::new();
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
                    AND (?2 IS NULL OR chunks.source_kind = ?2)
                    AND (?3 IS NULL OR chunks.field = ?3)
                ORDER BY rank, chunks.id
                LIMIT ?4 OFFSET ?5
            "#,
        )
        .map_err(fts_err)?;
    let mut rows = stmt
        .query(params![
            match_query,
            kind,
            field,
            sql_count(limit),
            sql_count(offset)
        ])
        .map_err(fts_err)?;
    collect_hits(&mut rows, offset, &mut hits)?;
    Ok(hits)
}

/// SQLite binds integers as `i64`; saturate rather than wrap a huge count.
fn sql_count(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}

fn collect_hits(
    rows: &mut rusqlite::Rows<'_>,
    offset: usize,
    hits: &mut Vec<Bm25Hit>,
) -> Result<(), OrbitError> {
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
            rank: offset.saturating_add(hits.len()).saturating_add(1),
        });
    }
    Ok(())
}

pub(crate) fn fts_terms_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}
