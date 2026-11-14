//! Coordinated task document, history, artifact, and search-index writes.

use orbit_common::{NotFoundKind, OrbitError};
use orbit_search::{LexicalIndex, TaskSidecarSearchText};
use orbit_store::contracts::{
    TaskArtifactStoreBackend, TaskArtifactUpdateParams, TaskCreateParams, TaskDocumentStoreBackend,
    TaskDocumentUpdateParams, TaskHistoryStoreBackend, TaskHistoryUpdateParams, TaskRow,
    TaskStoreBackend,
};
use orbit_types::task::Task;

use super::params::TaskRecordUpdateParams;
use crate::context::OrbitStores;

/// Coordinated task-record writes over [`OrbitStores`], which the runtime
/// kernel crate owns.
pub(crate) trait TaskRecords {
    fn task_records(&self) -> TaskRecordService<'_>;
}

impl TaskRecords for OrbitStores {
    fn task_records(&self) -> TaskRecordService<'_> {
        TaskRecordService {
            store: self.tasks(),
            document: self.task_documents(),
            history: self.task_history(),
            artifact: self.task_artifacts(),
            lexical_index: self.lexical_index(),
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
    lexical_index: &'a LexicalIndex,
}

impl TaskRecordService<'_> {
    pub(crate) fn index_task(&self, task: &Task) {
        // A lane whose index opened read-only (write-free runtimes, a sandbox
        // that mounts the search db read-only) cannot index the task it just
        // saved. Skip without attempting the write: the open already warned
        // once, and the stale entry is repaired by the next
        // `orbit search reindex` from writable storage. [ORB-24077]
        if self.lexical_index.is_read_only() {
            orbit_common::tracing::debug!(
                target: "orbit.search.indexer",
                task_id = %task.id,
                "search index is read-only; task stays unindexed until the next reindex"
            );
            return;
        }
        // Index the sidecars a `Task` does not carry (comment bodies,
        // artifact paths) from the row this write settled, so search can
        // answer this task from the index alone. A row that cannot be read
        // (in flight, a backend without one) indexes the task fields only,
        // which records the lesser coverage and keeps the bundle-matcher
        // fallback for this task until a later write or reindex covers it.
        let sidecars = self
            .store
            .get_task_row(&task.id, true)
            .ok()
            .flatten()
            .map(|row| TaskSidecarSearchText {
                comments: row
                    .comments
                    .into_iter()
                    .map(|comment| comment.message)
                    .collect(),
                artifact_paths: row
                    .artifacts
                    .into_iter()
                    .map(|artifact| artifact.path)
                    .collect(),
            });
        match self
            .lexical_index
            .store()
            .and_then(|index| match &sidecars {
                Some(sidecars) => index.index_task_with_sidecar_text(task, sidecars),
                None => index.index_task(task),
            }) {
            Ok(_) => {}
            // A sandbox can deny the write without the open observing a
            // read-only mount (a provider-native sandbox under bare
            // fallback). That is the same stale-until-reindex outcome, not a
            // per-write alarm.
            Err(error) if error.is_readonly_or_access_failure() => {
                orbit_common::tracing::debug!(
                    target: "orbit.search.indexer",
                    task_id = %task.id,
                    %error,
                    "search index refused the write; task stays unindexed until the next reindex"
                );
            }
            Err(error) => {
                orbit_common::tracing::warn!(task_id = %task.id, %error,
                    "task saved but search indexing failed; run orbit search reindex to repair");
            }
        }
    }

    pub(crate) fn create(&self, params: TaskCreateParams) -> Result<Task, OrbitError> {
        let task = self.store.create_task(params)?;
        self.index_task(&task);
        Ok(task)
    }

    pub(crate) fn create_with_key(
        &self,
        params: TaskCreateParams,
        key: Option<&str>,
    ) -> Result<(Task, bool), OrbitError> {
        let (task, replayed) = match key {
            Some(key) => self.store.create_task_idempotent(params, key)?,
            None => return self.create(params).map(|task| (task, false)),
        };
        self.index_task(&task);
        Ok((task, replayed))
    }

    pub(crate) fn create_guarded(
        &self,
        params: TaskCreateParams,
        key: Option<&str>,
        digest: Option<&str>,
    ) -> Result<(Task, bool), OrbitError> {
        if let (Some(key), Some(digest)) = (key, digest) {
            let result = self.store.create_desktop_task(params, key, digest)?;
            self.index_task(&result.0);
            Ok(result)
        } else {
            self.create_with_key(params, key)
        }
    }

    pub(crate) fn update(
        &self,
        id: &str,
        params: TaskRecordUpdateParams,
    ) -> Result<Task, OrbitError> {
        Ok(self.update_returning_row(id, params)?.task)
    }

    /// [`Self::update`], keeping the post-write row a document or history
    /// commit already holds so the response is not another bundle read.
    /// Artifact-only and no-op writes still read the row once.
    pub(crate) fn update_returning_row(
        &self,
        id: &str,
        params: TaskRecordUpdateParams,
    ) -> Result<TaskRow, OrbitError> {
        #[cfg(test)]
        if FAIL_NEXT_UPDATE.with(|fail| fail.replace(false)) {
            return Err(OrbitError::Execution(
                "injected task record update failure".to_string(),
            ));
        }
        // A work-starting write the claim footprint guard will refuse must
        // refuse before any companion artifact, document or history write:
        // the store's in-lock guard stays authoritative, and this pre-flight
        // keeps its deterministic refusal from leaving field edits behind.
        if params.has_history_changes() {
            self.history.check_ordinary_work_start_footprint(
                id,
                params.status,
                params.status_event.as_deref(),
                params.context_files.as_deref(),
            )?;
        }

        // Let the artifact store finish its rejection checks before publishing
        // document or status/history edits. Keeping those checks in the store
        // also covers reserved evidence paths and malformed coverage payloads.
        if params.has_artifact_changes() {
            self.artifact.upsert_task_artifacts(
                id,
                TaskArtifactUpdateParams {
                    origin: params.artifact_origin.clone(),
                    owner_run_id: params.artifact_owner_run_id.clone(),
                    writer: params.artifact_writer,
                    actor: params.actor.clone(),
                    upsert_artifacts: params.upsert_artifacts.clone(),
                },
            )?;
        }

        let attribution_in_history = params.attribution_joins_history_commit();
        // History runs after the document commit and its bundle includes both,
        // so the later row replaces the earlier one.
        let mut written = None;
        if params.has_document_changes() && !attribution_in_history {
            written = Some(self.document.update_task_document(
                id,
                TaskDocumentUpdateParams {
                    actor: params.actor.clone(),
                    title: params.title.clone(),
                    description: params.description.clone(),
                    acceptance_criteria: params.acceptance_criteria.clone(),
                    dependencies: params.dependencies.clone(),
                    relations: params.relations.clone(),
                    tags: params.tags.clone(),
                    granted_tools: params.granted_tools.clone(),
                    requires_labels: params.requires_labels.clone(),
                    not_before: params.not_before,
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
                    job_run_machine: params.job_run_machine.clone(),
                    crew: params.crew.clone(),
                    crew_source: params.crew_source.clone(),
                    orchestrator: params.orchestrator.clone(),
                    origin: params.origin,
                    context_creation: params.context_creation.clone(),
                },
            )?);
        }

        if params.has_history_changes() {
            written = Some(
                self.history.update_task_history(
                    id,
                    TaskHistoryUpdateParams {
                        actor: params.actor.clone(),
                        status: params.status,
                        status_event: params.status_event.clone(),
                        status_note: params.status_note.clone(),
                        status_decision: params.status_decision.clone(),
                        append_history: params.append_history.clone(),
                        append_comments: params.append_comments.clone(),
                        expected_status: params.expected_status.clone(),
                        operator_comment: params.operator_comment,
                        planned_by: params.planned_by.clone().filter(|_| attribution_in_history),
                        implemented_by: params
                            .implemented_by
                            .clone()
                            .filter(|_| attribution_in_history),
                    },
                )?,
            );
        }

        let row = match written {
            Some(row) => row,
            None => self
                .store
                .get_task_row(id, false)?
                .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))?,
        };
        if params.has_document_changes()
            || params.has_history_changes()
            || params.has_artifact_changes()
        {
            self.index_task(&row.task);
        }
        Ok(row)
    }

    pub(crate) fn delete(&self, id: &str) -> Result<bool, OrbitError> {
        let deleted = self.store.delete_task(id)?;
        // A workspace with no index has nothing to retract, so an unavailable
        // index reports here exactly like a failed cascade: the task is gone
        // either way, and the index reconciles on its next reindex.
        if deleted
            && let Err(error) = self
                .lexical_index
                .store()
                .and_then(|index| index.delete_source("task", id))
        {
            orbit_common::tracing::debug!(
                target: "orbit.search.indexer",
                task_id = id,
                error = %error,
                "search delete cascade failed after task deletion",
            );
        }
        Ok(deleted)
    }
}

#[cfg(test)]
thread_local! {
    /// Fails this thread's next [`TaskRecordService::update`] before it writes
    /// anything, as a store error would. Production builds do not have it.
    pub(super) static FAIL_NEXT_UPDATE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl crate::OrbitRuntime {
    /// Internal trusted invocation seam. Transport adapters must supply managed context,
    /// never construct it from tool arguments or advisory environment labels.
    pub fn mutate_execution_claim(
        &self,
        context: Option<&orbit_store::contracts::ClaimInvocation>,
        mutation_id: &str,
        mutation: &orbit_store::contracts::ClaimMutation,
    ) -> Result<orbit_store::contracts::ClaimMutationResult, OrbitError> {
        Ok(self
            .mutate_execution_claim_outcome(context, mutation_id, mutation)?
            .result)
    }

    /// [`Self::mutate_execution_claim`], also reporting whether this call
    /// committed the mutation rather than replaying its receipt: the only
    /// authority for a side effect that must follow the commit exactly once.
    pub(crate) fn mutate_execution_claim_outcome(
        &self,
        context: Option<&orbit_store::contracts::ClaimInvocation>,
        mutation_id: &str,
        mutation: &orbit_store::contracts::ClaimMutation,
    ) -> Result<orbit_store::contracts::ClaimMutationOutcome, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let outcome =
            self.stores()
                .tasks()
                .mutate_execution_claim_outcome(context, mutation_id, mutation)?;
        // A committed claim mutation writes searchable text (evidence
        // summary, comments, artifact paths, plan, external refs, and any
        // deferred branch observation it lands in the same commit) outside
        // `TaskRecordService::update`, so its recorded sidecar coverage
        // would otherwise vouch for chunks the index does not hold. Refresh
        // this task's chunks from its settled row before returning, so an
        // ordinary owner search sees the evidence without a reindex or a
        // later ordinary write. A replay committed nothing and leaves the
        // index untouched; indexing is best-effort, so a refresh failure
        // (read-only index, unreadable row) never fails the committed
        // mutation — the next reindex repairs it, as for ordinary writes.
        if outcome.committed
            && let Some(task_id) = context.map(|invocation| invocation.task_id())
            && let Ok(Some(task)) = self.stores().tasks().get_task(task_id)
        {
            self.stores().task_records().index_task(&task);
        }
        Ok(outcome)
    }

    /// Refuse a worker whose claim could no longer take its own update: the
    /// read-side fence that matches [`Self::mutate_execution_claim`]'s.
    pub(crate) fn verify_worker_claim(
        &self,
        context: &orbit_store::contracts::ClaimInvocation,
    ) -> Result<(), OrbitError> {
        self.stores().tasks().verify_worker_claim(context)
    }

    /// A claimed worker's mid-run steering records, behind the same fence
    /// as [`Self::verify_worker_claim`].
    pub(crate) fn worker_claim_steering(
        &self,
        context: &orbit_store::contracts::ClaimInvocation,
    ) -> Result<orbit_store::contracts::ClaimSteeringReading, OrbitError> {
        self.stores().tasks().worker_claim_steering(context)
    }

    pub fn inspect_execution_claims(
        &self,
    ) -> Result<Vec<orbit_store::contracts::ClaimInspection>, OrbitError> {
        self.stores().tasks().inspect_execution_claims()
    }

    /// Repairing counterpart of [`Self::inspect_execution_claims`] for live
    /// commands: an interrupted coordination commit is recovered first.
    pub fn resolve_execution_claims(
        &self,
    ) -> Result<Vec<orbit_store::contracts::ClaimInspection>, OrbitError> {
        self.stores().tasks().resolve_execution_claims()
    }

    /// Retain a CI receipt or defer it under a protecting claim.
    ///
    /// The store checks the current claim inside the same boundary that queues
    /// or retains the receipt. Settlement writes receipts still deferred.
    pub(crate) fn record_deferred_branch_observation(
        &self,
        observation: &orbit_store::contracts::DeferredBranchObservation,
    ) -> Result<orbit_store::contracts::BranchObservationOutcome, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let outcome = self
            .stores()
            .tasks()
            .record_deferred_branch_observation(observation)?;
        // A retained receipt lands an artifact (and maybe a comment)
        // outside `update_task`, and both are indexed search fields now:
        // refresh the owner's chunks or its recorded sidecar coverage
        // would vouch for text the index does not hold. A deferred
        // receipt has written nothing yet; settlement lands it inside a
        // claim commit, and the claim seam in
        // `mutate_execution_claim_outcome` re-indexes that commit before
        // it returns, so no later ordinary write is needed.
        if matches!(
            outcome,
            orbit_store::contracts::BranchObservationOutcome::Retained
        ) && let Ok(Some(task)) = self.stores().tasks().get_task(&observation.task_id)
        {
            self.stores().task_records().index_task(&task);
        }
        Ok(outcome)
    }
}
