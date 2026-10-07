use orbit_common::OrbitError;
use rusqlite::{params_from_iter, types::Value};

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
    /// Query terms matched by this chunk, using the same FTS5 tokenizer.
    pub matched_terms: usize,
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
    query_page(store, query, kind, field, offset, limit, false)
}

/// Any-term page ordered by matched-term count descending, then BM25 and
/// chunk id. The first occurrence of a source is its best matching chunk.
pub fn bm25_or_page(
    store: &LexicalStore,
    query: &str,
    kind: Option<&str>,
    field: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<Vec<Bm25Hit>, OrbitError> {
    query_page(store, query, kind, field, offset, limit, true)
}

#[allow(clippy::too_many_arguments)]
fn query_page(
    store: &LexicalStore,
    query: &str,
    kind: Option<&str>,
    field: Option<&str>,
    offset: usize,
    limit: usize,
    any_term: bool,
) -> Result<Vec<Bm25Hit>, OrbitError> {
    let terms = quoted_terms(query);
    if terms.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let match_query = if any_term {
        fts_or_terms_query(query)
    } else {
        fts_terms_query(query)
    };
    let mut bindings = vec![
        Value::Text(match_query),
        kind.map_or(Value::Null, |kind| Value::Text(kind.into())),
        field.map_or(Value::Null, |field| Value::Text(field.into())),
        Value::Integer(sql_count(limit)),
        Value::Integer(sql_count(offset)),
    ];
    // Count with FTS itself: substring checks would disagree on punctuation,
    // case, diacritics and whole-token boundaries. Each IN subquery is an
    // indexed term lookup within this statement, not another page query.
    let matched_terms = if any_term {
        let counts = terms.iter().enumerate().map(|(index, term)| {
            bindings.push(Value::Text(term.clone()));
            format!(
                "(corpus_fts.rowid IN (SELECT rowid FROM corpus_fts WHERE corpus_fts MATCH ?{}))",
                index + 6
            )
        }).collect::<Vec<_>>();
        counts.join(" + ")
    } else {
        terms.len().to_string()
    };
    let conn = store.connection();
    let conn = conn
        .lock()
        .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
    let fts_err = |error| crate::lexical::migration::translate_corpus_fts_sql_error(&conn, error);
    let sql = format!(
        "SELECT chunks.source_kind, chunks.source_id, chunks.field, chunks.id,
                bm25(corpus_fts) AS rank, ({matched_terms}) AS matched_terms
         FROM corpus_fts JOIN chunks ON chunks.id = corpus_fts.rowid
         WHERE corpus_fts MATCH ?1
           AND (?2 IS NULL OR chunks.source_kind = ?2)
           AND (?3 IS NULL OR chunks.field = ?3)
         ORDER BY matched_terms DESC, rank, chunks.id LIMIT ?4 OFFSET ?5"
    );
    let mut stmt = conn.prepare(&sql).map_err(fts_err)?;
    store.record_fts_query();
    let mut rows = stmt.query(params_from_iter(bindings)).map_err(fts_err)?;
    let mut hits = Vec::new();
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
            matched_terms: row
                .get(5)
                .map_err(|error| OrbitError::Store(error.to_string()))?,
        });
    }
    Ok(())
}

fn quoted_terms(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect()
}

pub(crate) fn fts_terms_query(query: &str) -> String {
    quoted_terms(query).join(" ")
}

fn fts_or_terms_query(query: &str) -> String {
    quoted_terms(query).join(" OR ")
}
