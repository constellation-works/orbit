//! Canonical command surface for orbit-search operations.
//!
//! Tool adapters and CLI delegates parse request envelopes, call into these
//! commands, and shape the returned typed results for their transport. The
//! top-level `OrbitRuntime` exposes thin delegates that build the runtime's
//! shared state into these calls.

mod doc_index;
mod doc_search;
mod install;
mod reindex;
mod related;
mod search;
mod stats;
mod uninstall;

pub use doc_index::{DocIndexParams, DocIndexResult};
pub use doc_search::{DocSemanticHit, DocSemanticSearchParams, DocSemanticSearchResult};
pub use install::{SemanticInstallParams, SemanticInstallResult};
pub use reindex::{
    IndexKind, SemanticIndexParams, SemanticIndexResult, SemanticReindexParams,
    SemanticReindexResult, TaskIndexResult,
};
pub use related::{SemanticRelatedParams, SemanticRelatedResult};
pub use search::{ScoreBreakdown, SemanticHit, SemanticSearchParams, SemanticSearchResult};
pub use stats::{CompanionStatus, SemanticStatsResult};
pub use uninstall::{SemanticUninstallParams, SemanticUninstallResult};

use std::fs;

use orbit_common::OrbitError;
use orbit_types::task::Task;

use crate::vector::{DocEmbeddingSource, VectorStore};
use crate::{CompanionPaths, ModelSpec, default_model};

pub(crate) const DEFAULT_RELEASE_BASE_URL: &str =
    "https://github.com/constellation-works/orbit/releases/latest/download";

pub(crate) fn parse_model(model: Option<&str>) -> Result<ModelSpec, OrbitError> {
    match model {
        Some(value) => ModelSpec::parse(value),
        None => Ok(default_model()),
    }
}

pub(crate) fn resolve_query_model(model: Option<&str>) -> Result<ModelSpec, OrbitError> {
    if model.is_some() {
        return parse_model(model);
    }
    let active = CompanionPaths::default_under_home()
        .ok()
        .and_then(|paths| active_model(&paths));
    parse_model(active.as_deref())
}

/// The `model_id` a query-side embedder will report for this host.
///
/// The companion stamps `model_id` with the model alias, so the alias is the
/// same key the `embeddings` rows are written under. A federated read compares
/// this against each workspace's indexed models [ORB-11027].
pub fn query_model_id(model: Option<&str>) -> Result<String, OrbitError> {
    resolve_query_model(model).map(|spec| spec.alias.to_string())
}

pub(crate) fn active_model(paths: &CompanionPaths) -> Option<String> {
    fs::read_to_string(&paths.active_model_path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(crate) fn remove_file_if_exists(path: &std::path::Path) -> Result<bool, OrbitError> {
    if path.exists() {
        fs::remove_file(path).map_err(|error| OrbitError::Io(error.to_string()))?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn semantic_install(
    params: SemanticInstallParams,
) -> Result<SemanticInstallResult, OrbitError> {
    install::run(params)
}

pub fn semantic_uninstall(
    params: SemanticUninstallParams,
) -> Result<SemanticUninstallResult, OrbitError> {
    uninstall::run(params)
}

pub fn semantic_index(
    vector_store: &VectorStore,
    tasks: &[Task],
    params: SemanticIndexParams,
) -> Result<TaskIndexResult, OrbitError> {
    reindex::run(vector_store, tasks, params)
}

pub fn semantic_reindex(
    vector_store: &VectorStore,
    tasks: &[Task],
    params: SemanticReindexParams,
) -> Result<SemanticReindexResult, OrbitError> {
    semantic_index(vector_store, tasks, params)
}

pub fn doc_index(
    vector_store: &VectorStore,
    docs: &[DocEmbeddingSource],
    params: DocIndexParams,
) -> Result<DocIndexResult, OrbitError> {
    doc_index::run(vector_store, docs, params)
}

pub fn semantic_stats(
    vector_store: &VectorStore,
    task_ids: &[String],
) -> Result<SemanticStatsResult, OrbitError> {
    stats::run(vector_store, task_ids)
}

pub fn semantic_search(
    vector_store: &VectorStore,
    params: SemanticSearchParams,
) -> Result<SemanticSearchResult, OrbitError> {
    search::run(vector_store, params)
}

pub fn doc_semantic_search(
    vector_store: &VectorStore,
    params: DocSemanticSearchParams,
) -> Result<DocSemanticSearchResult, OrbitError> {
    doc_search::run(vector_store, params)
}

pub fn semantic_related(
    vector_store: &VectorStore,
    tasks: &[Task],
    params: SemanticRelatedParams,
) -> Result<SemanticRelatedResult, OrbitError> {
    related::run(vector_store, tasks, params)
}

#[cfg(test)]
mod tests;
