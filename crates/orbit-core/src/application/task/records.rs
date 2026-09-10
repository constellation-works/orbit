//! Coordinated task document, history, artifact, and search-index writes.

use orbit_common::{NotFoundKind, OrbitError};
use orbit_search::{EmbedWorker, SemanticIndex};
use orbit_store::contracts::{
    TaskArtifactStoreBackend, TaskArtifactUpdateParams, TaskCreateParams, TaskDocumentStoreBackend,
    TaskDocumentUpdateParams, TaskHistoryStoreBackend, TaskHistoryUpdateParams, TaskStoreBackend,
};
use orbit_types::task::Task;

use super::params::TaskRecordUpdateParams;
use crate::context::OrbitStores;

impl OrbitStores {
    pub(crate) fn task_records(&self) -> TaskRecordService<'_> {
        TaskRecordService {
            store: self.tasks(),
            document: self.task_documents(),
            history: self.task_history(),
            artifact: self.task_artifacts(),
            semantic_index: self.semantic_index(),
            semantic_worker: self.semantic_worker(),
        }
    }
}

/// Coordinates task writes that span multiple persistence and indexing services.
///
/// Read-only calls use the typed backends exposed by [`OrbitStores`] directly.
pub(crate) struct TaskRecordService<'a> {
    store: &'a dyn TaskStoreBackend,
    document: &'a dyn TaskDocumentStoreBackend,
    history: &'a dyn TaskHistoryStoreBackend,
    artifact: &'a dyn TaskArtifactStoreBackend,
    semantic_index: &'a SemanticIndex,
    semantic_worker: &'a EmbedWorker,
}

impl TaskRecordService<'_> {
    pub(crate) fn create(&self, params: TaskCreateParams) -> Result<Task, OrbitError> {
        let task = self.store.create_task(params)?;
        self.semantic_worker.enqueue(task.clone());
        Ok(task)
    }

    pub(crate) fn create_with_key(
        &self,
        params: TaskCreateParams,
        key: Option<&str>,
    ) -> Result<Task, OrbitError> {
        let task = match key {
            Some(key) => self.store.create_task_idempotent(params, key)?,
            None => return self.create(params),
        };
        self.semantic_worker.enqueue(task.clone());
        Ok(task)
    }

    pub(crate) fn update(
        &self,
        id: &str,
        params: TaskRecordUpdateParams,
    ) -> Result<Task, OrbitError> {
        if params.has_document_changes() {
            self.document.update_task_document(
                id,
                TaskDocumentUpdateParams {
                    actor: params.actor.clone(),
                    title: params.title.clone(),
                    description: params.description.clone(),
                    acceptance_criteria: params.acceptance_criteria.clone(),
                    dependencies: params.dependencies.clone(),
                    relations: params.relations.clone(),
                    tags: params.tags.clone(),
                    plan: params.plan.clone(),
                    execution_summary: params.execution_summary.clone(),
                    context_files: params.context_files.clone(),
                    created_by: params.created_by.clone(),
                    planned_by: params.planned_by.clone(),
                    implemented_by: params.implemented_by.clone(),
                    priority: params.priority,
                    complexity: params.complexity,
                    task_type: params.task_type,
                    external_refs: params.external_refs.clone(),
                    pr_status: params.pr_status.clone(),
                    source_task_id: params.source_task_id.clone(),
                    job_run_id: params.job_run_id.clone(),
                    crew: params.crew.clone(),
                    orchestrator: params.orchestrator.clone(),
                },
            )?;
        }

        if params.has_history_changes() {
            self.history.update_task_history(
                id,
                TaskHistoryUpdateParams {
                    actor: params.actor.clone(),
                    status: params.status,
                    status_event: params.status_event.clone(),
                    status_note: params.status_note.clone(),
                    append_history: params.append_history.clone(),
                    append_comments: params.append_comments.clone(),
                    expected_status: params.expected_status.clone(),
                },
            )?;
        }

        if params.has_artifact_changes() {
            self.artifact.upsert_task_artifacts(
                id,
                TaskArtifactUpdateParams {
                    owner_run_id: params.artifact_owner_run_id.clone(),
                    actor: params.actor.clone(),
                    upsert_artifacts: params.upsert_artifacts.clone(),
                },
            )?;
        }

        let task = self
            .store
            .get_task(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))?;
        if params.has_document_changes()
            || params.has_history_changes()
            || params.has_artifact_changes()
        {
            self.semantic_worker.enqueue(task.clone());
        }
        Ok(task)
    }

    pub(crate) fn delete(&self, id: &str) -> Result<bool, OrbitError> {
        let deleted = self.store.delete_task(id)?;
        // A workspace with no index has nothing to retract, so an unavailable
        // index reports here exactly like a failed cascade: the task is gone
        // either way, and the index reconciles on its next reindex.
        if deleted
            && let Err(error) = self
                .semantic_index
                .store()
                .and_then(|vector| vector.delete_source("task", id))
        {
            orbit_common::tracing::debug!(
                target: "orbit.search.indexer",
                task_id = id,
                error = %error,
                "semantic delete cascade failed after task deletion",
            );
        }
        Ok(deleted)
    }
}
