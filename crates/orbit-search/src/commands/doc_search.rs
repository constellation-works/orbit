use std::collections::BTreeMap;

use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

use crate::commands::resolve_query_model;
use crate::vector::VectorStore;
use crate::vector::query::{CosineHit, cosine_top_k, snippet_for_hit};
use crate::vector::store::SOURCE_KIND_DOC;
use crate::{Embedder, SubprocessEmbedder};

const DEFAULT_LIMIT: usize = 10;
const RETRIEVER_OVERFETCH: usize = 4;
const SNIPPET_MAX_CHARS: usize = 280;

#[derive(Debug, Clone)]
pub struct DocSemanticSearchParams {
    pub query: String,
    pub limit: usize,
    pub model: Option<String>,
}

impl DocSemanticSearchParams {
    pub fn normalized_limit(&self) -> usize {
        if self.limit == 0 {
            DEFAULT_LIMIT
        } else {
            self.limit
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocSemanticSearchResult {
    pub results: Vec<DocSemanticHit>,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DocSemanticHit {
    pub source_id: String,
    pub best_field: String,
    pub snippet: String,
    pub score: f32,
}

pub fn run(
    vector_store: &VectorStore,
    params: DocSemanticSearchParams,
) -> Result<DocSemanticSearchResult, OrbitError> {
    let model = resolve_query_model(params.model.as_deref())?;
    let embedder = SubprocessEmbedder::with_model(model.alias)?;
    run_with_embedder(vector_store, &embedder, params)
}

pub(crate) fn run_with_embedder(
    vector_store: &VectorStore,
    embedder: &dyn Embedder,
    params: DocSemanticSearchParams,
) -> Result<DocSemanticSearchResult, OrbitError> {
    let query = params.query.trim();
    if query.is_empty() {
        return Err(OrbitError::InvalidInput(
            "doc semantic search query must not be empty".to_string(),
        ));
    }

    let vectors = embedder.embed(&[query])?;
    let query_vector = vectors.into_iter().next().ok_or_else(|| {
        OrbitError::Execution("embedder returned no vector for doc semantic search".to_string())
    })?;
    let limit = params.normalized_limit();
    let retriever_limit = limit.saturating_mul(RETRIEVER_OVERFETCH).max(limit);
    let model_id = embedder.model_id().to_string();
    let cosine = cosine_top_k(
        vector_store,
        &query_vector,
        &model_id,
        retriever_limit,
        Some(SOURCE_KIND_DOC),
        None,
    )?;
    let hits = rollup_doc_hits(vector_store, cosine, limit)?;

    Ok(DocSemanticSearchResult {
        results: hits,
        model_id,
    })
}

fn rollup_doc_hits(
    vector_store: &VectorStore,
    hits: Vec<CosineHit>,
    limit: usize,
) -> Result<Vec<DocSemanticHit>, OrbitError> {
    let mut best = BTreeMap::<String, CosineHit>::new();
    for hit in hits {
        best.entry(hit.source_id.clone())
            .and_modify(|current| {
                if crate::vector::query::cosine::compare_cosine_hits(&hit, current).is_lt() {
                    *current = hit.clone();
                }
            })
            .or_insert(hit);
    }

    let mut rolled = Vec::new();
    for hit in best.into_values() {
        let snippet = snippet_for_hit(
            vector_store,
            SOURCE_KIND_DOC,
            &hit.source_id,
            &hit.field,
            Some(hit.chunk_idx),
            None,
        )?
        .unwrap_or_default();
        rolled.push(DocSemanticHit {
            source_id: hit.source_id,
            best_field: hit.field,
            snippet: truncate_snippet(&snippet),
            score: hit.score,
        });
    }
    rolled.sort_by(compare_doc_semantic_hits);
    rolled.truncate(limit);
    Ok(rolled)
}

fn compare_doc_semantic_hits(left: &DocSemanticHit, right: &DocSemanticHit) -> std::cmp::Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.source_id.cmp(&right.source_id))
        .then_with(|| left.best_field.cmp(&right.best_field))
}

fn truncate_snippet(snippet: &str) -> String {
    let trimmed = snippet.trim();
    let mut end = 0;
    for (idx, ch) in trimmed.char_indices() {
        if idx > SNIPPET_MAX_CHARS {
            break;
        }
        end = idx + ch.len_utf8();
    }
    if end >= trimmed.len() {
        trimmed.to_string()
    } else {
        format!("{}...", trimmed[..end].trim_end())
    }
}
