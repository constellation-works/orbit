use std::cmp::Ordering;
use std::collections::BinaryHeap;

use orbit_common::OrbitError;
use rusqlite::params;

use crate::vector::store::VectorStore;
use crate::vector::store::schema::embeddings_has_normalized_column;
use crate::vector::{cosine_similarity_blob, dot_product_blob, l2_norm};

#[derive(Debug, Clone, PartialEq)]
pub struct CosineHit {
    pub source_kind: String,
    pub source_id: String,
    pub field: String,
    pub chunk_idx: usize,
    pub score: f32,
    pub rank: usize,
}

/// Max-heap ordered so the current worst top-k hit sits at the root.
/// `Ord` matches [`compare_cosine_hits`]: best is `Less`, worst is `Greater`.
struct HeapHit(CosineHit);

impl PartialEq for HeapHit {
    fn eq(&self, other: &Self) -> bool {
        compare_cosine_hits(&self.0, &other.0) == Ordering::Equal
    }
}

impl Eq for HeapHit {}

impl PartialOrd for HeapHit {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HeapHit {
    fn cmp(&self, other: &Self) -> Ordering {
        compare_cosine_hits(&self.0, &other.0)
    }
}

const COSINE_SQL: &str = r#"
    SELECT source_kind, source_id, field, chunk_idx, embedding, normalized
    FROM embeddings
    WHERE model_id = ?1
        AND (?2 IS NULL OR source_kind = ?2)
        AND (?3 IS NULL OR field = ?3)
"#;

const COSINE_SQL_LEGACY: &str = r#"
    SELECT source_kind, source_id, field, chunk_idx, embedding
    FROM embeddings
    WHERE model_id = ?1
        AND (?2 IS NULL OR source_kind = ?2)
        AND (?3 IS NULL OR field = ?3)
"#;

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

    let has_normalized = embeddings_has_normalized_column(&conn)?;
    let query_norm = l2_norm(query);
    let mut candidates: BinaryHeap<HeapHit> = BinaryHeap::with_capacity(limit);
    let mut stmt = conn
        .prepare(if has_normalized {
            COSINE_SQL
        } else {
            COSINE_SQL_LEGACY
        })
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let mut rows = stmt
        .query(params![model_id, kind, field])
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    while let Some(row) = rows
        .next()
        .map_err(|error| OrbitError::Store(error.to_string()))?
    {
        let score = row_score(row, query, query_norm, has_normalized)?;
        let should_retain = if candidates.len() < limit {
            true
        } else if let Some(worst) = candidates.peek() {
            match score.total_cmp(&worst.0.score) {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => compare_row_to_hit(row, &worst.0)? == Ordering::Less,
            }
        } else {
            true
        };
        if !should_retain {
            continue;
        }

        if candidates.len() == limit {
            candidates.pop();
        }
        candidates.push(HeapHit(row_to_hit(row, score)?));
    }

    let mut hits = Vec::with_capacity(candidates.len());
    hits.extend(candidates.into_iter().map(|hit| hit.0));
    hits.sort_by(compare_cosine_hits);
    for (idx, hit) in hits.iter_mut().enumerate() {
        hit.rank = idx + 1;
    }
    Ok(hits)
}

fn row_score(
    row: &rusqlite::Row<'_>,
    query: &[f32],
    query_norm: f32,
    has_normalized: bool,
) -> Result<f32, OrbitError> {
    let embedding_blob = row
        .get_ref(4)
        .map_err(|error| OrbitError::Store(error.to_string()))?
        .as_blob()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    let stored_normalized = has_normalized
        && row
            .get::<_, i64>(5)
            .map_err(|error| OrbitError::Store(error.to_string()))?
            != 0;
    if stored_normalized {
        if query_norm == 0.0 {
            return Ok(0.0);
        }
        return Ok(dot_product_blob(query, embedding_blob)? / query_norm);
    }
    cosine_similarity_blob(query, embedding_blob)
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
