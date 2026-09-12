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
use super::lifecycle::{FORCED_STATUS_EVENT, ensure_status_change_allowed};
use super::paths::{
    canonicalize_context_files_for_read, context_files_pruned_history_entry,
    context_workspace_root, normalize_context_files_for_write,
};
use orbit_types::task::TaskUpdateParams;

/// Which lifecycle rules a status change on this write must satisfy
/// [ORB-12245].
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum StatusAuthority {
    /// In-process callers that own their own transition rules: the delivery
    /// pipeline's activities, which compare-and-set against the status they
    /// observed, and Core use cases such as [`OrbitRuntime::archive_task`].
    #[default]
    Internal,
    /// Attributed operator and agent surfaces — the CLI `task update`, the
    /// dashboard, and the registered `orbit.task.update` tool. The lifecycle
    /// table decides.
    Lifecycle,
    /// A human overriding the table on the bare CLI, recorded in task history
    /// as [`FORCED_STATUS_EVENT`].
    Forced,
}

#[derive(Default)]
struct TaskUpdateContext {
    status_note: Option<String>,
    actor_override: Option<String>,
    agent: Option<String>,
    model: Option<String>,
    artifact_owner: Option<String>,
    expected_status: Option<TaskStatus>,
    status_authority: StatusAuthority,
}

impl OrbitRuntime {
    /// The in-crate task setter. Status changes are *not* checked against the
    /// lifecycle table here: callers are Core's own use cases, which either
    /// change no status or own the transition themselves. Everything outside
    /// this crate goes through a guarded entry point below.
    pub(crate) fn update_task(
        &self,
        id: &str,
        params: TaskUpdateParams,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(id, params, TaskUpdateContext::default())
    }

    pub fn update_task_with_identity(
        &self,
        id: &str,
        params: TaskUpdateParams,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                agent,
                model,
                status_authority: StatusAuthority::Lifecycle,
                ..Default::default()
            },
        )
    }

    /// Apply a dashboard-authored mutation under an explicit human label.
    /// Agent-facing callers use `update_task_with_identity`, whose provenance
    /// is validated as a canonical agent family.
    pub fn update_task_as_human(
        &self,
        id: &str,
        params: TaskUpdateParams,
        actor_label: String,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                actor_override: Some(actor_label),
                status_authority: StatusAuthority::Lifecycle,
                ..Default::default()
            },
        )
    }

    /// The human escape hatch behind `orbit task update --force`: apply the
    /// update even when the lifecycle table refuses the status change, and
    /// record the override in task history.
    ///
    /// Only the bare CLI reaches this. The registered `orbit.task.update` tool
    /// refuses a `force` argument outright, so no agent can grant itself the
    /// override.
    pub fn force_update_task_with_identity(
        &self,
        id: &str,
        params: TaskUpdateParams,
        agent: Option<String>,
        model: Option<String>,
    ) -> Result<Task, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                agent,
                model,
                status_authority: StatusAuthority::Forced,
                ..Default::default()
            },
        )
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
        self.update_task_with_context(
            id,
            params,
            TaskUpdateContext {
                agent,
                model,
                artifact_owner: owner,
                status_authority: StatusAuthority::Lifecycle,
                ..Default::default()
            },
        )
    }

    pub fn update_task_from_activity(
        &self,
        id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let TaskActivityUpdate {
            status,
            expected_status,
            execution_summary,
            comment,
            note,
            agent,
            model,
        } = update;
        self.update_task_with_context(
            id,
            TaskUpdateParams {
                execution_summary,
                comment,
                status: Some(status),
                ..Default::default()
            },
            TaskUpdateContext {
                status_note: note,
                actor_override: Some(SYSTEM_ACTOR_LABEL.to_string()),
                agent,
                model,
                expected_status: Some(expected_status),
                ..Default::default()
            },
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
    fn update_task_with_context(
        &self,
        id: &str,
        params: TaskUpdateParams,
        context: TaskUpdateContext,
    ) -> Result<Task, OrbitError> {
        // The lock hook takes `FnMut` because it is a trait object, but the
        // body must run exactly once and consumes its inputs; `take()` makes
        // both facts explicit rather than forcing the params to be cloneable.
        let mut inputs = Some((params, context));
        let mut updated: Option<Task> = None;
        self.stores().tasks().with_task_write_lock(id, &mut || {
            let (params, context) = inputs.take().ok_or_else(|| {
                OrbitError::Execution("task update body was invoked more than once".to_string())
            })?;
            updated = Some(self.update_task_locked(id, params, context)?);
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
        context: TaskUpdateContext,
    ) -> Result<Task, OrbitError> {
        let TaskUpdateContext {
            status_note,
            actor_override,
            agent,
            model,
            artifact_owner,
            expected_status,
            status_authority,
        } = context;
        let (canonical_agent, canonical_model) = match actor_override.as_ref() {
            Some(_) => crate::context::trusted_write_identity(agent.as_deref(), model.as_deref()),
            None => self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?,
        };
        let task = self.get_task(id)?;
        if let Some(expected_status) = expected_status
            && task.status != expected_status
        {
            return Err(OrbitError::InvalidInput(format!(
                "task '{id}' status changed to '{}' before this activity write; expected '{expected_status}'",
                task.status
            )));
        }
        let requested_status = params.status.filter(|status| *status != task.status);
        if let Some(target) = requested_status
            && status_authority == StatusAuthority::Lifecycle
        {
            ensure_status_change_allowed(self, &task, &params, target)?;
        }
        let prune_root = context_workspace_root(&self.paths().repo_root, None);

        let dropped_context_files: Vec<String> = if let Some(candidates) =
            params.context_files.take()
        {
            let normalized = normalize_context_files_for_write(candidates, &prune_root)?;
            // An explicit replacement preserves draft/future selectors; pruning stays read-time.
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
        if params.status == Some(TaskStatus::Done) && task.status != TaskStatus::Done {
            let mut preview = task.clone();
            if let Some(relations) = &params.relations {
                preview.relations = relations.clone();
            }
            self.ensure_resolves_are_workspace_local(&preview)?;
        }

        let actor = self.actor().clone();
        let attribution = assemble_task_attribution(
            &task,
            TaskAttributionInput {
                default_actor_label: &actor.label,
                actor_override: actor_override.as_deref(),
                agent: canonical_agent.as_deref(),
                model: canonical_model.as_deref(),
                runtime_model_identity: None,
                plan_changed: params.plan.is_some(),
                // A classification edit alone is not implementation evidence.
                // Activity writes carry an expected status and therefore come
                // from an execution boundary; ordinary edits must include a
                // summary before implementation attribution is inferred.
                target_status: (expected_status.is_some() || params.execution_summary.is_some())
                    .then_some(params.status)
                    .flatten(),
                explicit_planned_by: params.planned_by.as_ref(),
                explicit_implemented_by: params.implemented_by.as_ref(),
            },
        )?;
        let effective_label = attribution.actor.clone();
        let status_note = status_note
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let append_comments = build_task_comments(
            params.comment.clone(),
            attribution.authored_role_label.as_str(),
        )?;
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
        // A forced transition is still a transition: naming it in history is
        // what separates a human override from a governed lifecycle move.
        let status_event = (status_authority == StatusAuthority::Forced
            && requested_status.is_some())
        .then(|| FORCED_STATUS_EVENT.to_string());
        let previous_status = task.status;
        let updated = self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                id,
                TaskRecordUpdateParams {
                    artifact_owner_run_id: artifact_owner.clone(),
                    actor: effective_label.clone(),
                    planned_by: attribution.planned_by.clone(),
                    implemented_by: attribution.implemented_by.clone(),
                    status_event: status_event.clone(),
                    status_note,
                    append_comments: append_comments.clone(),
                    append_history: append_history.clone(),
                    expected_status: expected_status.map(|status| vec![status]),
                    ..TaskRecordUpdateParams::from(params)
                },
            )?;
            let event = if previous_status == TaskStatus::Archived
                && updated.status != TaskStatus::Archived
            {
                OrbitEvent::TaskUnarchived { id: id.to_string() }
            } else if previous_status != TaskStatus::Archived
                && updated.status == TaskStatus::Archived
            {
                OrbitEvent::TaskArchived { id: id.to_string() }
            } else {
                OrbitEvent::TaskUpdated { id: id.to_string() }
            };
            Ok((updated.clone(), event))
        })?;

        Ok(updated)
    }
}
