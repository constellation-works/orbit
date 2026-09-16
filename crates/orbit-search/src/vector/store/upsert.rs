//! BLAKE3-deduped per-field write path.
//!
//! `upsert_embeddings` is the single-source entry. Full-corpus callers use the
//! same implementation through `upsert_embedding_sources`: it snapshots every
//! stored source, chunks changed fields with no store lock held, embeds all
//! chunks in bounded cross-source batches, then writes bounded groups of
//! sources per transaction. Unchanged fields short-circuit via `content_hash`.
//! Concurrent same-source writers follow the conflict policy documented on
//! [`VectorStore::upsert_embeddings`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use orbit_common::OrbitError;
use rusqlite::{Connection, TransactionBehavior, params};

use super::VectorStore;
use crate::Embedder;
use crate::vector::chunker::chunk_text;
use crate::vector::{EmbeddingField, UpsertReport, encode_f32_blob, normalize_f32};

const TARGET_CHUNK_TOKENS: usize = 400;
const OVERLAP_TOKENS: usize = 50;
const EMBED_BATCH_CHUNKS: usize = 64;
const WRITE_BATCH_SOURCES: usize = 64;

impl VectorStore {
    /// Replace the indexed field set for a source.
    ///
    /// `fields` is the complete current field set, not a partial patch; rows
    /// for previously indexed fields absent from this slice are removed.
    ///
    /// Chunking and companion inference run **without** the connection mutex
    /// or a SQLite write transaction. Only the snapshot and the final commit
    /// take the mutex; the commit uses `BEGIN IMMEDIATE` so a second
    /// connection cannot interleave mid-write.
    ///
    /// # Concurrency
    ///
    /// The first complete same-source write (another `upsert_embeddings` or
    /// `delete_source`) that commits after this call's snapshot wins. Commit
    /// reloads the source's field names and active-model content hashes and
    /// aborts with [`OrbitError::Store`] unless that fingerprint is unchanged.
    /// The abort does not write, so it cannot mix field revisions, overwrite a
    /// later accepted source, or resurrect a deleted source. A hash recheck of
    /// only the fields this call prepared is not the commit gate.
    pub fn upsert_embeddings(
        &self,
        source_kind: &str,
        source_id: &str,
        fields: &[EmbeddingField],
        embedder: &dyn Embedder,
        force: bool,
    ) -> Result<UpsertReport, OrbitError> {
        self.upsert_embedding_sources(source_kind, &[(source_id, fields)], embedder, force)
    }

    /// Replace several complete source field sets through bounded inference
    /// and write batches.
    ///
    /// Every source is fingerprinted before inference and rechecked in the
    /// transaction that writes it. A conflict rolls back that whole write
    /// batch; earlier bounded batches may already have committed.
    pub(super) fn upsert_embedding_sources(
        &self,
        source_kind: &str,
        sources: &[(&str, &[EmbeddingField])],
        embedder: &dyn Embedder,
        force: bool,
    ) -> Result<UpsertReport, OrbitError> {
        let snapshots = {
            let conn = self.connection();
            let conn = lock_connection(&conn)?;
            sources
                .iter()
                .map(|(source_id, _)| {
                    StoredSource::load(&conn, source_kind, source_id, embedder.model_id())
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut report = UpsertReport::default();
        let mut prepared_sources = Vec::new();
        for ((source_id, fields), snapshot) in sources.iter().zip(snapshots) {
            let expected_fields = fields
                .iter()
                .map(|field| field.field.as_str())
                .collect::<BTreeSet<_>>();
            let stale_fields = snapshot
                .fields_outside(&expected_fields)
                .map(str::to_string)
                .collect::<Vec<_>>();

            let mut empty_fields = Vec::new();
            let mut prepared_fields = Vec::new();
            for field in *fields {
                if field.text.trim().is_empty() {
                    empty_fields.push(field.field.clone());
                    continue;
                }
                let field_hash = content_hash(&field.text);
                if !force && snapshot.content_hash_matches(&field.field, &field_hash) {
                    report.skipped_fields += 1;
                    continue;
                }
                prepared_fields.push(prepare_field_chunks(field, &field_hash, embedder)?);
            }

            let prepared = PreparedSource {
                source_id: (*source_id).to_string(),
                snapshot,
                stale_fields,
                empty_fields,
                fields: prepared_fields,
            };
            if prepared.has_writes() {
                prepared_sources.push(prepared);
            }
        }

        embed_prepared_sources(&mut prepared_sources, embedder)?;
        report.embedded_chunks = prepared_sources
            .iter()
            .flat_map(|source| &source.fields)
            .map(|field| field.chunks.len())
            .sum();

        if prepared_sources.is_empty() {
            return Ok(report);
        }

        let conn = self.connection();
        let mut conn = lock_connection(&conn)?;
        for source_batch in prepared_sources.chunks(WRITE_BATCH_SOURCES) {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            for source in source_batch {
                let current =
                    StoredSource::load(&tx, source_kind, &source.source_id, embedder.model_id())?;
                if current != source.snapshot {
                    return Err(source_changed_during_embed(source_kind, &source.source_id));
                }

                for field in &source.stale_fields {
                    delete_field_rows(&tx, source_kind, &source.source_id, field, None)?;
                }
                for field in &source.empty_fields {
                    delete_field_rows(
                        &tx,
                        source_kind,
                        &source.source_id,
                        field,
                        Some(embedder.model_id()),
                    )?;
                }
                for field in &source.fields {
                    delete_field_rows(
                        &tx,
                        source_kind,
                        &source.source_id,
                        &field.name,
                        Some(embedder.model_id()),
                    )?;
                    insert_field_chunks(&tx, source_kind, &source.source_id, field, embedder)?;
                }
            }

            tx.commit()
                .map_err(|error| OrbitError::Store(error.to_string()))?;
        }

        Ok(report)
    }
}

fn lock_connection(
    conn: &Arc<Mutex<Connection>>,
) -> Result<MutexGuard<'_, Connection>, OrbitError> {
    conn.lock()
        .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))
}

fn source_changed_during_embed(source_kind: &str, source_id: &str) -> OrbitError {
    OrbitError::Store(format!(
        "source {source_kind}:{source_id} changed during embedding; upsert aborted"
    ))
}

/// Chunk one field's text with no store access. Inference is deferred until
/// chunks from every changed field and source can share bounded batches.
fn prepare_field_chunks(
    field: &EmbeddingField,
    field_hash: &str,
    embedder: &dyn Embedder,
) -> Result<PreparedField, OrbitError> {
    let chunks = chunk_text(
        &field.text,
        embedder,
        TARGET_CHUNK_TOKENS.min(embedder.max_input_tokens()),
        OVERLAP_TOKENS,
    )?;
    Ok(PreparedField {
        name: field.field.clone(),
        hash: field_hash.to_string(),
        chunks,
        vectors: Vec::new(),
    })
}

fn embed_prepared_sources(
    sources: &mut [PreparedSource],
    embedder: &dyn Embedder,
) -> Result<(), OrbitError> {
    let text_refs = sources
        .iter()
        .flat_map(|source| &source.fields)
        .flat_map(|field| field.chunks.iter().map(String::as_str))
        .collect::<Vec<_>>();
    let mut vectors = Vec::with_capacity(text_refs.len());
    for batch in text_refs.chunks(EMBED_BATCH_CHUNKS) {
        let batch_vectors = embedder.embed(batch)?;
        if batch_vectors.len() != batch.len() {
            return Err(OrbitError::Execution(format!(
                "embedder returned {} vectors for {} chunks",
                batch_vectors.len(),
                batch.len()
            )));
        }
        for vector in &batch_vectors {
            if vector.len() != embedder.dim() {
                return Err(OrbitError::Execution(format!(
                    "embedder returned dim {} but advertised {}",
                    vector.len(),
                    embedder.dim()
                )));
            }
        }
        vectors.extend(batch_vectors);
    }
    drop(text_refs);

    let mut vectors = vectors.into_iter();
    for field in sources.iter_mut().flat_map(|source| &mut source.fields) {
        field.vectors = vectors.by_ref().take(field.chunks.len()).collect();
    }
    debug_assert!(vectors.next().is_none());
    Ok(())
}

struct PreparedSource {
    source_id: String,
    snapshot: StoredSource,
    stale_fields: Vec<String>,
    empty_fields: Vec<String>,
    fields: Vec<PreparedField>,
}

impl PreparedSource {
    fn has_writes(&self) -> bool {
        !self.stale_fields.is_empty() || !self.empty_fields.is_empty() || !self.fields.is_empty()
    }
}

struct PreparedField {
    name: String,
    hash: String,
    chunks: Vec<String>,
    vectors: Vec<Vec<f32>>,
}

/// Store one already-embedded field. The caller has already removed the
/// field's previous rows.
fn insert_field_chunks(
    conn: &Connection,
    source_kind: &str,
    source_id: &str,
    field: &PreparedField,
    embedder: &dyn Embedder,
) -> Result<usize, OrbitError> {
    for (idx, (chunk, vector)) in field.chunks.iter().zip(field.vectors.iter()).enumerate() {
        conn.prepare_cached(
            r#"
                INSERT INTO embeddings(
                    source_kind, source_id, field, chunk_idx, content_hash,
                    model_id, dim, embedding, created_at, normalized
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(source_kind, source_id, field, chunk_idx, model_id)
                DO UPDATE SET
                    content_hash = excluded.content_hash,
                    dim = excluded.dim,
                    embedding = excluded.embedding,
                    created_at = excluded.created_at,
                    normalized = excluded.normalized
            "#,
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .execute(params![
            source_kind,
            source_id,
            field.name,
            idx as i64,
            field.hash,
            embedder.model_id(),
            embedder.dim() as i64,
            encode_f32_blob(&normalize_f32(vector)),
            now_string(),
            1_i64,
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
            field.name,
            idx as i64,
            chunk
        ])
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    Ok(field.chunks.len())
}

/// What one source already has indexed, read once per `upsert_embeddings`.
///
/// Both halves are answered by a single `(source_kind, source_id)` index
/// search. That matters because every reindex — including the common one where
/// nothing changed — asks these questions for every source; the
/// pre-[ORB-11695] layout answered them with a scan of the whole corpus, which
/// made a reindex quadratic in corpus size.
#[derive(PartialEq, Eq)]
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
        for hashes in stored.hashes.values_mut() {
            hashes.sort();
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
