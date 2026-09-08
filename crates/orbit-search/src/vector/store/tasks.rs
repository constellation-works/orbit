//! Task-corpus indexing entry points.
//!
//! `index_task` and `reindex_tasks` are the convenience wrappers that wire
//! `task_embedding_fields(...)` (per-field extraction) into `upsert_embeddings`.

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
        let mut upsert = UpsertReport::default();
        let live = tasks
            .iter()
            .map(|task| task.id.clone())
            .collect::<BTreeSet<_>>();
        for task in tasks {
            let report = self.index_task(task, embedder, force)?;
            upsert.embedded_chunks += report.embedded_chunks;
            upsert.skipped_fields += report.skipped_fields;
        }

        let mut stale_sources = Vec::new();
        for source_id in self.source_ids(SOURCE_KIND_TASK)? {
            if !live.contains(&source_id) {
                self.delete_source(SOURCE_KIND_TASK, &source_id)?;
                stale_sources.push(source_id);
            }
        }
        Ok(TaskReindexReport {
            upsert,
            stale_sources,
        })
    }
}
