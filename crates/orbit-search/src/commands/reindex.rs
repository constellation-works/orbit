use orbit_common::OrbitError;
use orbit_types::task::Task;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::commands::doc_index::DocIndexResult;
use crate::commands::parse_model;
use crate::vector::{UpsertReport, VectorStore};
use crate::{Embedder, SubprocessEmbedder};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndexKind {
    #[default]
    Tasks,
    Docs,
    All,
}

impl FromStr for IndexKind {
    type Err = OrbitError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw {
            "tasks" => Ok(Self::Tasks),
            "docs" => Ok(Self::Docs),
            "all" => Ok(Self::All),
            value => Err(OrbitError::InvalidInput(format!(
                "unsupported semantic index kind `{value}`; supported values: tasks, docs, all"
            ))),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SemanticIndexParams {
    pub model: Option<String>,
    pub force: bool,
    pub kind: Option<IndexKind>,
}

impl SemanticIndexParams {
    pub fn resolved_kind(&self) -> IndexKind {
        self.kind.unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskIndexResult {
    pub model_id: String,
    pub report: UpsertReport,
    /// Task sources dropped because they are no longer in the live corpus.
    pub stale_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum SemanticIndexResult {
    Tasks {
        model_id: String,
        report: UpsertReport,
        stale_sources: Vec<String>,
    },
    Docs {
        model_id: String,
        report: UpsertReport,
        indexed_sources: usize,
        stale_sources: Vec<String>,
    },
    All {
        tasks: TaskIndexResult,
        docs: DocIndexResult,
    },
}

impl From<TaskIndexResult> for SemanticIndexResult {
    fn from(result: TaskIndexResult) -> Self {
        Self::Tasks {
            model_id: result.model_id,
            report: result.report,
            stale_sources: result.stale_sources,
        }
    }
}

impl From<DocIndexResult> for SemanticIndexResult {
    fn from(result: DocIndexResult) -> Self {
        Self::Docs {
            model_id: result.model_id,
            report: result.report,
            indexed_sources: result.indexed_sources,
            stale_sources: result.stale_sources,
        }
    }
}

pub type SemanticReindexParams = SemanticIndexParams;
pub type SemanticReindexResult = TaskIndexResult;

pub fn run(
    vector_store: &VectorStore,
    tasks: &[Task],
    params: SemanticIndexParams,
) -> Result<TaskIndexResult, OrbitError> {
    let model = parse_model(params.model.as_deref())?;
    let embedder = SubprocessEmbedder::with_model(model.alias)?;
    run_with_embedder(vector_store, tasks, &embedder, params.force)
}

pub(crate) fn run_with_embedder(
    vector_store: &VectorStore,
    tasks: &[Task],
    embedder: &dyn Embedder,
    force: bool,
) -> Result<TaskIndexResult, OrbitError> {
    let report = vector_store.reindex_tasks(tasks, embedder, force)?;
    Ok(TaskIndexResult {
        model_id: embedder.model_id().to_string(),
        report: report.upsert,
        stale_sources: report.stale_sources,
    })
}
