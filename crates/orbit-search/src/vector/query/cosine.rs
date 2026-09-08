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

    let mut candidates: Vec<CosineHit> = Vec::with_capacity(limit);
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
        let score = row_score(row, query)?;
        let should_retain = if candidates.len() < limit {
            true
        } else {
            let worst = match candidates.last() {
                Some(worst) => worst,
                None => unreachable!("a candidate list at the limit is non-empty"),
            };
            match score.total_cmp(&worst.score) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => {
                    compare_row_to_hit(row, worst)? == std::cmp::Ordering::Less
                }
            }
        };
        if !should_retain {
            continue;
        }

        candidates.push(row_to_hit(row, score)?);
        candidates.sort_by(compare_cosine_hits);
        if candidates.len() > limit {
            candidates.pop();
        }
    }

    for (idx, hit) in candidates.iter_mut().enumerate() {
        hit.rank = idx + 1;
    }
    Ok(candidates)
}

fn row_score(row: &rusqlite::Row<'_>, query: &[f32]) -> Result<f32, OrbitError> {
    let embedding_blob = row
        .get_ref(4)
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .as_blob()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let embedding = decode_f32_blob(embedding_blob)?;
    cosine_similarity(query, &embedding)
}

fn row_to_hit(row: &rusqlite::Row<'_>, score: f32) -> Result<CosineHit, OrbitError> {
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

fn compare_row_to_hit(
    row: &rusqlite::Row<'_>,
    right: &CosineHit,
) -> Result<std::cmp::Ordering, OrbitError> {
    let source_kind = row
        .get_ref(0)
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .as_str()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let source_id = row
        .get_ref(1)
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .as_str()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let field = row
        .get_ref(2)
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .as_str()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let chunk_idx = row
        .get_ref(3)
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .as_i64()
        .map_err(|error| OrbitError::Store(error.to_string()))? as usize;

    Ok(source_kind
        .cmp(&right.source_kind)
        .then_with(|| source_id.cmp(&right.source_id))
        .then_with(|| field.cmp(&right.field))
        .then_with(|| chunk_idx.cmp(&right.chunk_idx)))
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
