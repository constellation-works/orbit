use orbit_common::OrbitError;
use rusqlite::params;

use crate::vector::store::VectorStore;
use crate::vector::{cosine_similarity, decode_f32_blob};

#[derive(Debug, Clone, PartialEq)]
pub struct CosineHit {
    pub source_kind: String,
    pub source_id: String,
    pub field: String,
    pub chunk_idx: usize,
    pub score: f32,
    pub rank: usize,
}

pub fn cosine_top_k(
    store: &VectorStore,
    query: &[f32],
    model_id: &str,
    limit: usize,
    kind: Option<&str>,
    field: Option<&str>,
) -> Result<Vec<CosineHit>, OrbitError> {
    if query.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let conn = store.connection();
    let conn = conn
        .lock()
        .map_err(|error| OrbitError::Store(format!("mutex poisoned: {error}")))?;

    let mut candidates = Vec::new();
    let mut stmt = conn
        .prepare(
            r#"
                SELECT source_kind, source_id, field, chunk_idx, embedding
                FROM embeddings
                WHERE model_id = ?1
                    AND (?2 IS NULL OR source_kind = ?2)
                    AND (?3 IS NULL OR field = ?3)
            "#,
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let mut rows = stmt
        .query(params![model_id, kind, field])
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| OrbitError::Store(error.to_string()))?
    {
        candidates.push(row_to_hit(row, query)?);
    }

    candidates.sort_by(compare_cosine_hits);
    candidates.truncate(limit);
    for (idx, hit) in candidates.iter_mut().enumerate() {
        hit.rank = idx + 1;
    }
    Ok(candidates)
}

fn row_to_hit(row: &rusqlite::Row<'_>, query: &[f32]) -> Result<CosineHit, OrbitError> {
    let embedding_blob: Vec<u8> = row
        .get(4)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let embedding = decode_f32_blob(&embedding_blob)?;
    let score = cosine_similarity(query, &embedding)?;
    Ok(CosineHit {
        source_kind: row
            .get(0)
            .map_err(|error| OrbitError::Store(error.to_string()))?,
        source_id: row
            .get(1)
            .map_err(|error| OrbitError::Store(error.to_string()))?,
        field: row
            .get(2)
            .map_err(|error| OrbitError::Store(error.to_string()))?,
        chunk_idx: row
            .get::<_, i64>(3)
            .map_err(|error| OrbitError::Store(error.to_string()))? as usize,
        score,
        rank: 0,
    })
}

pub(crate) fn compare_cosine_hits(left: &CosineHit, right: &CosineHit) -> std::cmp::Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.source_kind.cmp(&right.source_kind))
        .then_with(|| left.source_id.cmp(&right.source_id))
        .then_with(|| left.field.cmp(&right.field))
        .then_with(|| left.chunk_idx.cmp(&right.chunk_idx))
}
