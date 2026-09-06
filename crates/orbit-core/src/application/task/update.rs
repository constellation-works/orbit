use orbit_common::OrbitError;
use orbit_common::fs::task_io::prune_missing_context_files;
use orbit_engine::TaskActivityUpdate;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    Task, TaskHistoryEntry, TaskStatus, normalize_task_dependencies, normalize_task_tags,
    validate_task_dependencies,
};

use super::TaskRecordUpdateParams;
use crate::OrbitRuntime;

use super::helpers::{
    SYSTEM_ACTOR_LABEL, TaskAttributionInput, assemble_task_attribution, build_task_comments,
    describe_optional_field_value,
};
use super::params::TaskUpdateParams;
use super::paths::{
    canonicalize_context_files_for_read, context_files_pruned_history_entry,
    context_workspace_root, normalize_context_files_for_write,
};
use super::transitions::{ensure_task_has_execution_plan, in_progress_transition_requires_plan};

impl OrbitRuntime {
    pub fn update_task(&self, id: &str, params: TaskUpdateParams) -> Result<Task, OrbitError> {
        self.update_task_with_identity(id, params, None, None)
    }

    pub fn update_task_with_identity(
        &self,
        id: &str,
        params: TaskUpdateParams,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_status_note_and_identity(id, params, None, agent, model, None)
    }

    pub(crate) fn update_task_with_owner(
        &self,
        id: &str,
        params: TaskUpdateParams,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_status_note_and_identity(id, params, None, agent, model, owner)
    }

    pub fn update_task_from_activity(
        &self,
        id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let TaskActivityUpdate {
            status,
            execution_summary,
            comment,
            note,
            agent,
            model,
        } = update;
        self.update_task_with_status_note_and_identity(
            id,
            TaskUpdateParams {
                execution_summary,
                comment,
                status: Some(status),
                ..Default::default()
            },
            note,
            agent.or_else(|| model.is_none().then(|| SYSTEM_ACTOR_LABEL.to_string())),
            model,
            None,
        )
    }

    /// Apply an update, holding the task's write lock across the whole
    /// read-modify-write.
    ///
    /// ORB-10988: the body below reads the task, derives history entries and
    /// status-transition validity from that snapshot, and only then writes. The
    /// store locks each write, but not the read that decided it — so two
    /// concurrent updates to the same task each validated against the same
    /// pre-state and the later write silently discarded the earlier one. The
    /// lock is re-entrant per thread, so the store's own per-write locking
    /// still holds underneath this one.
    fn update_task_with_status_note_and_identity(
        &self,
        id: &str,
        params: TaskUpdateParams,
        status_note: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<String>,
    ) -> Result<Task, OrbitError> {
        // The lock hook takes `FnMut` because it is a trait object, but the
        // body must run exactly once and consumes its inputs; `take()` makes
        // both facts explicit rather than forcing the params to be cloneable.
        let mut inputs = Some((params, status_note, agent, model, owner));
        let mut updated: Option<Task> = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let (params, status_note, agent, model, owner) = inputs.take().ok_or_else(|| {
                OrbitError::Execution("task update body was invoked more than once".to_string())
            })?;
            updated =
                Some(self.update_task_locked(id, params, status_note, agent, model, owner)?);
            Ok(())
        })?;
        let updated = updated.ok_or_else(|| {
            OrbitError::Execution("task update body did not run under the task lock".to_string())
        })?;

        // Cascading friction/task resolution touches *other* records, so it
        // stays outside this task's lock.
        if updated.status == TaskStatus::Done {
            self.record_resolves_side_effects(&updated)?;
        }
        Ok(updated)
    }

    fn update_task_locked(
        &self,
        id: &str,
        mut params: TaskUpdateParams,
        status_note: Option<String>,
        agent: Option<String>,
        model: Option<String>,
        owner: Option<String>,
    ) -> Result<Task, OrbitError> {
        let (canonical_agent, canonical_model) =
            self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let task = self.get_task(id)?;
        let prune_root = context_workspace_root(&self.paths().repo_root, None);

        let dropped_context_files: Vec<String> = if let Some(candidates) =
            params.context_files.take()
        {
            let normalized = normalize_context_files_for_write(candidates, &prune_root)?;
            // L-0030: explicit replacements preserve draft/future selectors; pruning stays read-time.
            params.context_files = Some(normalized);
            Vec::new()
        } else {
            let normalized = canonicalize_context_files_for_read(&task.context_files, &prune_root);
            if normalized != task.context_files {
                let (kept, dropped) = prune_missing_context_files(&prune_root, normalized);
                params.context_files = Some(kept);
                dropped
            } else {
                Vec::new()
            }
        };
        if let Some(dependencies) = params.dependencies.take() {
            let normalized_dependencies = normalize_task_dependencies(dependencies)?;
            validate_task_dependencies(&self.list_tasks()?, Some(id), &normalized_dependencies)?;
            params.dependencies = Some(normalized_dependencies);
        }
        if let Some(tags) = params.tags.take() {
            params.tags = Some(normalize_task_tags(tags));
        }
        if let Some(crew) = &params.crew {
            self.validate_crew_name(crew.as_deref())?;
        }
        if let Some(orchestrator) = &mut params.orchestrator {
            *orchestrator = self.canonical_crew_name(orchestrator.as_deref())?;
            if !matches!(task.status, TaskStatus::Proposed | TaskStatus::Backlog) {
                return Err(OrbitError::InvalidInput(format!(
                    "task {id} is {}; orchestrator can only be changed while proposed or backlog",
                    task.status
                )));
            }
        }
        // Archived tasks accept exactly one mutation: the guarded restore to
        // backlog (formerly `orbit task unarchive`). Everything else requires
        // restoring the task first.
        let unarchiving =
            task.status == TaskStatus::Archived && params.status == Some(TaskStatus::Backlog);
        if params.has_any_mutation() && task.status == TaskStatus::Archived && !unarchiving {
            return Err(OrbitError::InvalidInput(format!(
                "task {id} is {} and cannot be modified; restore it with `orbit task update {id} --status backlog` first",
                task.status
            )));
        }
        if params.has_non_comment_mutation() && task.status == TaskStatus::Done {
            return Err(OrbitError::InvalidInput(format!(
                "task {id} is {} and cannot be modified; done is terminal",
                task.status
            )));
        }

        if let Some(target_status) = params.status {
            if target_status == TaskStatus::Archived {
                return Err(OrbitError::InvalidInput(
                    "use `orbit task archive <id>` instead of setting status to archived"
                        .to_string(),
                ));
            }
            task.status
                .validate_transition(target_status)
                .map_err(OrbitError::TaskStatusTransition)?;
            if target_status == TaskStatus::InProgress
                && task.status != TaskStatus::InProgress
                && in_progress_transition_requires_plan(task.status)
            {
                let effective_plan = params.plan.as_deref().unwrap_or(task.plan.as_str());
                ensure_task_has_execution_plan(id, effective_plan)?;
            }
            if target_status == TaskStatus::Done && task.status != TaskStatus::Done {
                let mut preview = task.clone();
                if let Some(relations) = &params.relations {
                    preview.relations = relations.clone();
                }
                self.ensure_resolves_are_workspace_local(&preview)?;
            }
        }

        if task.status == TaskStatus::InProgress && params.status == Some(TaskStatus::Review) {
            let effective_execution_summary = params
                .execution_summary
                .as_deref()
                .unwrap_or(task.execution_summary.as_str());
            if effective_execution_summary.trim().is_empty() {
                return Err(OrbitError::InvalidInput(format!(
                    "task '{id}' requires non-empty execution_summary before transitioning in-progress -> review"
                )));
            }
        }

        let actor = self.actor().clone();
        let attribution = assemble_task_attribution(
            &task,
            TaskAttributionInput {
                default_actor_label: &actor.label,
                actor_override: None,
                agent: canonical_agent.as_deref(),
                model: canonical_model.as_deref(),
                runtime_model_identity: None,
                plan_changed: params.plan.is_some(),
                target_status: params.status,
                explicit_planned_by: params.planned_by.as_ref(),
                explicit_implemented_by: params.implemented_by.as_ref(),
            },
        );
        let effective_label = attribution.actor;
        let status_note = status_note
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let append_comments =
            build_task_comments(params.comment.clone(), effective_label.as_str())?;
        // ORB-10311: a persisted task comment no longer emits a bare `commented`
        // history stub; the comment itself (append_comments) is the record.
        let source_task_id_replacement = params
            .source_task_id
            .as_ref()
            .map(|value| value.as_deref())
            .filter(|replacement| task.source_task_id() != *replacement);

        let mut append_history: Vec<TaskHistoryEntry> = if dropped_context_files.is_empty() {
            Vec::new()
        } else {
            vec![context_files_pruned_history_entry(
                effective_label.as_str(),
                &dropped_context_files,
            )]
        };
        // An explicit block is current intent even when the failure already
        // parked the task. Triage must not mistake this for its old coupling.
        if task.status == TaskStatus::Blocked && params.status == Some(TaskStatus::Blocked) {
            append_history.push(TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: effective_label.clone(),
                event: "block_confirmed".into(),
                note: params.comment.clone(),
                from_status: Some(TaskStatus::Blocked),
                to_status: Some(TaskStatus::Blocked),
            });
        }
        if let Some(replacement) = source_task_id_replacement {
            // ORB-10311: record the explicit previous and replacement source
            // ids (with a clear marker for the unset case) so the change is
            // auditable from history alone.
            append_history.push(TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: effective_label.clone(),
                event: "updated".to_string(),
                note: Some(format!(
                    "source_task_id changed: {} → {}",
                    describe_optional_field_value(task.source_task_id()),
                    describe_optional_field_value(replacement),
                )),
                from_status: None,
                to_status: None,
            });
        }
        let updated = self.with_mutation(|| {
            let task = self.stores().task_records().update(
                id,
                TaskRecordUpdateParams {
                    artifact_owner_run_id: owner.clone(),
                    actor: effective_label.clone(),
                    planned_by: attribution.planned_by.clone(),
                    implemented_by: attribution.implemented_by.clone(),
                    status_note,
                    append_comments: append_comments.clone(),
                    append_history: append_history.clone(),
                    ..TaskRecordUpdateParams::from(params)
                },
            )?;
            let event = if unarchiving {
                OrbitEvent::TaskUnarchived { id: id.to_string() }
            } else {
                OrbitEvent::TaskUpdated { id: id.to_string() }
            };
            Ok((task.clone(), event))
        })?;

        Ok(updated)
    }
}
