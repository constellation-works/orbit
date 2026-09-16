//! Task-corpus indexing entry points.
//!
//! `index_task` and `reindex_tasks` are the convenience wrappers that wire
//! `task_embedding_fields(...)` (per-field extraction) into the single- and
//! multi-source upsert paths.

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_types::task::Task;

use super::{SOURCE_KIND_TASK, VectorStore};
use crate::Embedder;
use crate::vector::UpsertReport;
use crate::vector::task_fields::task_embedding_fields;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReindexReport {
    pub upsert: UpsertReport,
    pub stale_sources: Vec<String>,
}

impl VectorStore {
    pub fn index_task(
        &self,
        task: &Task,
        embedder: &dyn Embedder,
        force: bool,
    ) -> Result<UpsertReport, OrbitError> {
        self.upsert_embeddings(
            SOURCE_KIND_TASK,
            &task.id,
            &task_embedding_fields(task),
            embedder,
            force,
        )
    }

    /// Rebuild the task corpus: upsert every live task, then drop the rows of
    /// task sources that are no longer live.
    ///
    /// The delete sweep is the only way to clear rows the `task.delete`
    /// cascade never removed — deletions taken while this index was read-only
    /// or unopened, imported or restored state, and cascades that failed. It
    /// is what makes the `stale_rows` count reported by `stats` recoverable
    /// without deleting `semantic.db`.
    pub fn reindex_tasks(
        &self,
        tasks: &[Task],
        embedder: &dyn Embedder,
        force: bool,
    ) -> Result<TaskReindexReport, OrbitError> {
        let live = tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<BTreeSet<_>>();
        let stale_sources = self
            .source_ids(SOURCE_KIND_TASK)?
            .difference(&live)
            .cloned()
            .collect::<Vec<_>>();
        let mut sources = tasks
            .iter()
            .map(|task| (task.id.clone(), task_embedding_fields(task)))
            .collect::<Vec<_>>();
        sources.extend(
            stale_sources
                .iter()
                .cloned()
                .map(|source_id| (source_id, Vec::new())),
        );
        let source_refs = sources
            .iter()
            .map(|(source_id, fields)| (source_id.as_str(), fields.as_slice()))
            .collect::<Vec<_>>();
        let upsert =
            self.upsert_embedding_sources(SOURCE_KIND_TASK, &source_refs, embedder, force)?;

        Ok(TaskReindexReport {
            upsert,
            stale_sources,
        })
    }
}
