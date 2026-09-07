//! BLAKE3-deduped per-field write path.
//!
//! `upsert_embeddings` is the canonical entry: it transactionally chunks each
//! field's text, embeds the chunks, and writes the resulting rows into both
//! `embeddings` (vector storage) and `chunks` (the FTS5 content table, which
//! its triggers mirror into `corpus_fts`). Unchanged fields short-circuit via
//! `content_hash`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use rusqlite::{Connection, params};

use super::VectorStore;
use crate::Embedder;
use crate::vector::chunker::chunk_text;
use crate::vector::{EmbeddingField, UpsertReport, encode_f32_blob};

const TARGET_CHUNK_TOKENS: usize = 400;
const OVERLAP_TOKENS: usize = 50;

impl VectorStore {
    /// Replace the indexed field set for a source.
    ///
    /// `fields` is the complete current field set, not a partial patch; rows
    /// for previously indexed fields absent from this slice are removed.
    pub fn upsert_embeddings(
        &self,
        source_kind: &str,
        source_id: &str,
        fields: &[EmbeddingField],
        embedder: &dyn Embedder,
        force: bool,
    ) -> Result<UpsertReport, OrbitError> {
        let mut report = UpsertReport::default();
        let conn = self.connection();
        let mut conn = conn
            .lock()
            .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;
        let tx = conn
            .transaction()
            .map_err(|error| OrbitError::Store(error.to_string()))?;

        let stored = StoredSource::load(&tx, source_kind, source_id, embedder.model_id())?;
        let expected_fields = fields
            .iter()
            .map(|field| field.field.as_str())
            .collect::<BTreeSet<_>>();
        for field in stored.fields_outside(&expected_fields) {
            delete_field_rows(&tx, source_kind, source_id, field, None)?;
        }

        for field in fields {
            if field.text.trim().is_empty() {
                delete_field_rows(
                    &tx,
                    source_kind,
                    source_id,
                    &field.field,
                    Some(embedder.model_id()),
                )?;
                continue;
            }
            let field_hash = content_hash(&field.text);
            if !force && stored.content_hash_matches(&field.field, &field_hash) {
                report.skipped_fields += 1;
                continue;
            }

            delete_field_rows(
                &tx,
                source_kind,
                source_id,
                &field.field,
                Some(embedder.model_id()),
            )?;
            report.embedded_chunks +=
                write_field_chunks(&tx, source_kind, source_id, field, &field_hash, embedder)?;
        }

        tx.commit()
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        Ok(report)
    }
}

/// Chunk, embed and store one field's text, returning the number of chunks
/// written. The caller has already removed the field's previous rows.
fn write_field_chunks(
    conn: &Connection,
    source_kind: &str,
    source_id: &str,
    field: &EmbeddingField,
    field_hash: &str,
    embedder: &dyn Embedder,
) -> Result<usize, OrbitError> {
    let chunks = chunk_text(
        &field.text,
        embedder,
        TARGET_CHUNK_TOKENS.min(embedder.max_input_tokens()),
        OVERLAP_TOKENS,
    )?;
    let text_refs = chunks.iter().map(String::as_str).collect::<Vec<_>>();
    let vectors = embedder.embed(&text_refs)?;
    if vectors.len() != chunks.len() {
        return Err(OrbitError::Execution(format!(
            "embedder returned {} vectors for {} chunks",
            vectors.len(),
            chunks.len()
        )));
    }

    for (idx, (chunk, vector)) in chunks.iter().zip(vectors.iter()).enumerate() {
        if vector.len() != embedder.dim() {
            return Err(OrbitError::Execution(format!(
                "embedder returned dim {} but advertised {}",
                vector.len(),
                embedder.dim()
            )));
        }
        conn.prepare_cached(
            r#"
                INSERT INTO embeddings(
                    source_kind, source_id, field, chunk_idx, content_hash,
                    model_id, dim, embedding, created_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                ON CONFLICT(source_kind, source_id, field, chunk_idx, model_id)
                DO UPDATE SET
                    content_hash = excluded.content_hash,
                    dim = excluded.dim,
                    embedding = excluded.embedding,
                    created_at = excluded.created_at
            "#,
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .execute(params![
            source_kind,
            source_id,
            field.field,
            idx as i64,
            field_hash,
            embedder.model_id(),
            embedder.dim() as i64,
            encode_f32_blob(vector),
            now_string(),
        ])
        .map_err(|error| OrbitError::Store(error.to_string()))?;
        conn.prepare_cached(
            r#"
                INSERT INTO chunks(source_kind, source_id, field, chunk_idx, content)
                VALUES (?1, ?2, ?3, ?4, ?5)
                ON CONFLICT(source_kind, source_id, field, chunk_idx)
                DO UPDATE SET content = excluded.content
            "#,
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .execute(params![
            source_kind,
            source_id,
            field.field,
            idx as i64,
            chunk
        ])
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    Ok(chunks.len())
}

/// What one source already has indexed, read once per `upsert_embeddings`.
///
/// Both halves are answered by a single `(source_kind, source_id)` index
/// search. That matters because every reindex — including the common one where
/// nothing changed — asks these questions for every source; the
/// pre-[ORB-11695] layout answered them with a scan of the whole corpus, which
/// made a reindex quadratic in corpus size.
struct StoredSource {
    /// Fields with rows in `embeddings` (under any model) or in `chunks`. The
    /// two can differ: the legacy FTS migration backfills chunk text for
    /// fields that were never embedded.
    fields: BTreeSet<String>,
    /// Content hashes recorded for the active model, per field. A field with
    /// several chunks contributes one entry per chunk, all of the same hash.
    hashes: BTreeMap<String, Vec<String>>,
}

impl StoredSource {
    fn load(
        conn: &Connection,
        source_kind: &str,
        source_id: &str,
        model_id: &str,
    ) -> Result<Self, OrbitError> {
        let mut stmt = conn
            .prepare_cached(
                r#"
                    SELECT field, model_id, content_hash
                    FROM embeddings
                    WHERE source_kind = ?1 AND source_id = ?2
                    UNION ALL
                    SELECT DISTINCT field, NULL, NULL
                    FROM chunks
                    WHERE source_kind = ?1 AND source_id = ?2
                "#,
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map(params![source_kind, source_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|error| OrbitError::Store(error.to_string()))?;

        let mut stored = Self {
            fields: BTreeSet::new(),
            hashes: BTreeMap::new(),
        };
        for row in rows {
            let (field, row_model_id, content_hash) =
                row.map_err(|error| OrbitError::Store(error.to_string()))?;
            if let (Some(row_model_id), Some(content_hash)) = (row_model_id, content_hash)
                && row_model_id == model_id
            {
                stored
                    .hashes
                    .entry(field.clone())
                    .or_default()
                    .push(content_hash);
            }
            stored.fields.insert(field);
        }
        Ok(stored)
    }

    /// Indexed fields the caller no longer publishes — the rows to prune.
    fn fields_outside<'a>(&'a self, expected: &'a BTreeSet<&str>) -> impl Iterator<Item = &'a str> {
        self.fields
            .iter()
            .map(String::as_str)
            .filter(move |field| !expected.contains(field))
    }

    /// True when the field is already indexed for the active model and every
    /// one of its chunks was written from this exact text.
    fn content_hash_matches(&self, field: &str, expected_hash: &str) -> bool {
        self.hashes.get(field).is_some_and(|hashes| {
            !hashes.is_empty() && hashes.iter().all(|hash| hash == expected_hash)
        })
    }
}

/// Drop every indexed row for one field of one source.
///
/// `model_id` scopes only the vector rows: `Some(model)` keeps other models'
/// embeddings for the field, `None` removes the field outright. The chunk rows
/// are model-independent, so they always go, and the `corpus_fts` triggers
/// follow.
fn delete_field_rows(
    conn: &Connection,
    source_kind: &str,
    source_id: &str,
    field: &str,
    model_id: Option<&str>,
) -> Result<(), OrbitError> {
    match model_id {
        Some(model_id) => conn
            .prepare_cached(
                r#"
                    DELETE FROM embeddings
                    WHERE source_kind = ?1 AND source_id = ?2 AND field = ?3 AND model_id = ?4
                "#,
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?
            .execute(params![source_kind, source_id, field, model_id]),
        None => conn
            .prepare_cached(
                r#"
                    DELETE FROM embeddings
                    WHERE source_kind = ?1 AND source_id = ?2 AND field = ?3
                "#,
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?
            .execute(params![source_kind, source_id, field]),
    }
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    conn.prepare_cached(
        "DELETE FROM chunks WHERE source_kind = ?1 AND source_id = ?2 AND field = ?3",
    )
    .map_err(|error| OrbitError::Store(error.to_string()))?
    .execute(params![source_kind, source_id, field])
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    Ok(())
}

fn content_hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn now_string() -> String {
    Utc::now().to_rfc3339()
}
