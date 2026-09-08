use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

use crate::commands::resolve_query_model;
use crate::vector::VectorStore;
use crate::vector::query::{bm25_top_k, reciprocal_rank_fusion, rollup_to_tasks, snippet_for_hit};
use crate::{Embedder, SubprocessEmbedder};

const DEFAULT_LIMIT: usize = 10;
const RETRIEVER_OVERFETCH: usize = 4;
const SNIPPET_MAX_CHARS: usize = 280;

#[derive(Debug, Clone)]
pub struct SemanticSearchParams {
    pub query: String,
    pub limit: usize,
    pub field: Option<String>,
    pub kind: Option<String>,
    pub model: Option<String>,
}

impl SemanticSearchParams {
    pub fn normalized_limit(&self) -> usize {
        if self.limit == 0 {
            DEFAULT_LIMIT
        } else {
            self.limit
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticSearchResult {
    pub results: Vec<SemanticHit>,
    pub model_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticHit {
    pub source_kind: String,
    pub source_id: String,
    pub best_field: String,
    pub snippet: String,
    pub score: f32,
    pub score_breakdown: ScoreBreakdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreBreakdown {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rrf: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bm25_rank: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cosine_rank: Option<usize>,
}

pub fn run(
    vector_store: &VectorStore,
    params: SemanticSearchParams,
) -> Result<SemanticSearchResult, OrbitError> {
    let model = resolve_query_model(params.model.as_deref())?;
    let embedder = SubprocessEmbedder::with_model(model.alias)?;
    run_with_embedder(vector_store, &embedder, params)
}

pub(crate) fn run_with_embedder(
    vector_store: &VectorStore,
    embedder: &dyn Embedder,
    params: SemanticSearchParams,
) -> Result<SemanticSearchResult, OrbitError> {
    let query = params.query.trim();
    if query.is_empty() {
        return Err(OrbitError::InvalidInput(
            "semantic search query must not be empty".to_string(),
        ));
    }

    let vectors = embedder.embed(&[query])?;
    let query_vector = vectors.into_iter().next().ok_or_else(|| {
        OrbitError::Execution("embedder returned no vector for semantic search query".to_string())
    })?;
    let limit = params.normalized_limit();
    let retriever_limit = limit.saturating_mul(RETRIEVER_OVERFETCH).max(limit);
    let kind = params.kind.as_deref();
    let model_id = embedder.model_id().to_string();
    let field = params.field.as_deref();
    let cosine = crate::vector::query::cosine_top_k(
        vector_store,
        &query_vector,
        &model_id,
        retriever_limit,
        kind,
        field,
    )?;
    let bm25 = bm25_top_k(vector_store, query, kind, field, retriever_limit)?;

    let candidates = reciprocal_rank_fusion(&cosine, &bm25);
    let task_hits = rollup_to_tasks(candidates, limit);
    let results = task_hits
        .into_iter()
        .map(|hit| {
            let snippet = snippet_for_hit(
                vector_store,
                &hit.source_kind,
                &hit.source_id,
                &hit.best_field,
                hit.best_chunk_idx,
                hit.best_rowid,
            )?
            .unwrap_or_default();
            Ok(SemanticHit {
                source_kind: hit.source_kind,
                source_id: hit.source_id,
                best_field: hit.best_field,
                snippet: truncate_snippet(&snippet),
                score: hit.score,
                score_breakdown: ScoreBreakdown {
                    rrf: Some(hit.score),
                    bm25_rank: hit.bm25_rank,
                    cosine_rank: hit.cosine_rank,
                },
            })
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;

    Ok(SemanticSearchResult { results, model_id })
}

pub(crate) fn truncate_snippet(snippet: &str) -> String {
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
