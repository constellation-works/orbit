use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::Task;
use serde::{Deserialize, Serialize};

use crate::commands::resolve_query_model;
use crate::commands::search::{ScoreBreakdown, SemanticHit, truncate_snippet};
use crate::vector::VectorStore;
use crate::vector::query::{FusedCandidate, cosine_top_k, rollup_to_tasks, snippet_for_hit};
use crate::{Embedder, SubprocessEmbedder};

const DEFAULT_LIMIT: usize = 10;
const RETRIEVER_OVERFETCH: usize = 4;

#[derive(Debug, Clone)]
pub struct SemanticRelatedParams {
    pub task_id: String,
    pub limit: usize,
    pub model: Option<String>,
}

impl SemanticRelatedParams {
    pub fn normalized_limit(&self) -> usize {
        if self.limit == 0 {
            DEFAULT_LIMIT
        } else {
            self.limit
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SemanticRelatedResult {
    pub results: Vec<SemanticHit>,
    pub model_id: String,
}

pub fn run(
    vector_store: &VectorStore,
    tasks: &[Task],
    params: SemanticRelatedParams,
) -> Result<SemanticRelatedResult, OrbitError> {
    let model = resolve_query_model(params.model.as_deref())?;
    let embedder = SubprocessEmbedder::with_model(model.alias)?;
    run_with_embedder(vector_store, tasks, &embedder, params)
}

pub(crate) fn run_with_embedder(
    vector_store: &VectorStore,
    tasks: &[Task],
    embedder: &dyn Embedder,
    params: SemanticRelatedParams,
) -> Result<SemanticRelatedResult, OrbitError> {
    let target = tasks
        .iter()
        .find(|task| task.id == params.task_id)
        .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, params.task_id.clone()))?;
    let query = format!("{}\n\n{}", target.title.trim(), target.description.trim());
    let query = query.trim();
    if query.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "task {} has no embeddable title/description",
            target.id
        )));
    }

    let vectors = embedder.embed(&[query])?;
    let query_vector = vectors.into_iter().next().ok_or_else(|| {
        OrbitError::Execution("embedder returned no vector for semantic related query".to_string())
    })?;
    let limit = params.normalized_limit();
    let retriever_limit = limit.saturating_mul(RETRIEVER_OVERFETCH).max(limit + 1);
    let cosine = cosine_top_k(
        vector_store,
        &query_vector,
        embedder.model_id(),
        retriever_limit,
        Some("task"),
        None,
    )?;
    let candidates = cosine
        .into_iter()
        .filter(|hit| hit.source_id != target.id)
        .map(|hit| FusedCandidate {
            source_kind: hit.source_kind,
            source_id: hit.source_id,
            field: hit.field,
            chunk_idx_for_snippet: Some(hit.chunk_idx),
            rowid_for_snippet: None,
            score: hit.score,
            bm25_rank: None,
            cosine_rank: Some(hit.rank),
        })
        .collect::<Vec<_>>();
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
                    rrf: None,
                    bm25_rank: None,
                    cosine_rank: hit.cosine_rank,
                },
            })
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;

    Ok(SemanticRelatedResult {
        results,
        model_id: embedder.model_id().to_string(),
    })
}
