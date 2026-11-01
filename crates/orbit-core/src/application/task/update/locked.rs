use orbit_common::OrbitError;
use orbit_common::governance::authorization::agent_context_declared;
use orbit_common::security::redaction::redact_all;
use orbit_types::identity::normalize_attribution_label;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ArtifactWriter, CANDIDATE_DISCARDED_EVENT, FINAL_RECOVERY_DECIDED_EVENT,
    GRANTED_TOOLS_CHANGED_EVENT, RecordedFinalRecovery, Task, TaskDecision, TaskHistoryEntry,
    TaskStatus, canonical_artifact_path, granted_tools_change_note,
};
use orbit_types::workflow::{SOURCE_REPAIR_KEY, SOURCE_REPAIR_LINKED};
use serde_json::Value;

use super::super::TaskRecordUpdateParams;
use super::super::TaskRecords;
use super::super::helpers::{
    SYSTEM_ACTOR_LABEL, TaskAttributionInput, assemble_task_attribution, build_task_comments,
    describe_optional_field_value,
};
use super::super::lifecycle::{
    FORCED_STATUS_EVENT, HUMAN_CREW_BYPASS_NOTE, ensure_completion_run_stopped,
    ensure_post_deploy_criteria_met, ensure_status_change_allowed,
    ensure_status_change_not_run_owned,
};
use super::super::params::TaskUpdateParams;
use super::artifacts::admit_task_artifact_as;
use super::entry::{
    LockedTaskUpdate, StatusAuthority, TaskFieldWriter, TaskUpdateContext, task_field_writer,
};
use crate::OrbitRuntime;
use crate::application::task::{TaskJobPort, TaskReviewPort};

impl OrbitRuntime {
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
    pub(super) fn update_task_with_context(
        &self,
        id: &str,
        params: TaskUpdateParams,
        context: TaskUpdateContext,
    ) -> Result<Task, OrbitError> {
        // The lock hook takes `FnMut` because it is a trait object, but the
        // body must run exactly once and consumes its inputs; `take()` makes
        // both facts explicit rather than forcing the params to be cloneable.
        // An operator's or agent's own move back to backlog or someday may
        // also close the task's undelivered PRs; Orbit's own holds and
        // requeues keep them for the run that continues the candidate.
        let attributed_requeue = matches!(
            context.status_authority,
            StatusAuthority::Lifecycle | StatusAuthority::Forced
        ) && !context.system_writer;
        let discarded = params.discard_candidate;
        // [ORB-16723] An operator's comment-only update may land under an
        // execution claim. Its lock admits the claim; the write itself is
        // re-classified below and by the store, which refuses anything more.
        let operator_comment = context.operator_decision_authority
            && !context.system_writer
            && context.agent.is_none()
            && context.model.is_none()
            && params.is_comment_only();
        let mut inputs = Some((params, context));
        let mut updated: Option<LockedTaskUpdate> = None;
        let mut body = || {
            let (params, context) = inputs.take().ok_or_else(|| {
                OrbitError::Execution("task update body was invoked more than once".to_string())
            })?;
            updated = Some(self.update_task_locked(id, params, context)?);
            Ok(())
        };
        let tasks = self.stores().tasks();
        if operator_comment {
            tasks.with_task_comment_write_lock(id, &mut body)?;
        } else {
            tasks.with_task_write_lock(id, &mut body)?;
        }
        let LockedTaskUpdate {
            task: updated,
            previous_status,
            status_note,
        } = updated.ok_or_else(|| {
            OrbitError::Execution("task update body did not run under the task lock".to_string())
        })?;

        // Cascading friction/task resolution touches *other* records, so it
        // stays outside this task's lock.
        self.record_resolves_side_effects(previous_status, &updated);
        // So does the forge round trip that closes the task's PRs.
        self.close_task_prs_after_transition(previous_status, &updated, status_note.as_deref());
        if attributed_requeue {
            self.close_task_prs_after_requeue(
                previous_status,
                &updated,
                status_note.as_deref(),
                discarded,
            );
        }
        Ok(updated)
    }

    pub(super) fn update_task_locked(
        &self,
        id: &str,
        mut params: TaskUpdateParams,
        context: TaskUpdateContext,
    ) -> Result<LockedTaskUpdate, OrbitError> {
        let TaskUpdateContext {
            status_note,
            actor_override,
            agent,
            model,
            artifact_owner,
            expected_status,
            status_authority,
            calling_run_id,
            operator_decision_authority,
            system_writer,
            guard_work_start,
        } = context;
        let (canonical_agent, canonical_model) = match actor_override.as_ref() {
            Some(_) => crate::context::trusted_write_identity(agent.as_deref(), model.as_deref()),
            None => self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?,
        };
        let task = self.get_task(id)?;
        let comment_only = params.is_comment_only();
        // Gate records are certificates and authority, not agent claims.
        // Only the trusted system entry point may write them; an actor label
        // or artifact/tool field cannot opt into that authority. Normalize
        // before checking exactly the key the store will persist.
        // Only an operator surface may write under `mocks/` (§2.4): no
        // agent identity and no agent envelope, as `operator_write` below
        // derives from the same facts.
        let allow_mocks = operator_decision_authority
            && canonical_agent.is_none()
            && canonical_model.is_none()
            && !agent_context_declared();
        for artifact in &mut params.upsert_artifacts {
            if system_writer {
                artifact.path = canonical_artifact_path(&artifact.path)?;
            } else {
                admit_task_artifact_as(self, &task, artifact, allow_mocks)?;
            }
        }
        if let Some(expected_status) = expected_status
            && task.status != expected_status
        {
            return Err(OrbitError::InvalidInput(format!(
                "task '{id}' status changed to '{}' before this activity write; expected '{expected_status}'",
                task.status
            )));
        }
        let requested_status = params.status.filter(|status| *status != task.status);
        if requested_status.is_some()
            && status_authority == StatusAuthority::Lifecycle
            && !system_writer
        {
            ensure_status_change_not_run_owned(self, id)?;
        }
        // The label matches the one `operator_write` derives below. An agent
        // identity does not consult it, and resolving it would repeat work
        // the canonical pair already settled.
        let classification_label = if let Some(label) = actor_override.as_deref() {
            normalize_attribution_label(label, None)
        } else if canonical_agent.is_none() && canonical_model.is_none() {
            super::super::helpers::effective_actor_label(&self.actor().label, None, None)?
        } else {
            String::new()
        };
        let writer = task_field_writer(
            operator_decision_authority,
            canonical_agent.as_deref(),
            canonical_model.as_deref(),
            &classification_label,
        );
        // An operator moving a human crew's task is not held to the lifecycle
        // table or its plan, review, evidence and verdict gates: the operator
        // does that work outside any pipeline. Only an attributed operator
        // surface qualifies — the same writer class `--force` requires — so an
        // agent, an MCP caller or Orbit's own machinery keeps the lifecycle,
        // and assigning a task to a human crew is never a way to skip review.
        // The move out of `done` or `archived` keeps the force rules.
        let human_crew_bypass = if requested_status.is_some()
            && status_authority == StatusAuthority::Lifecycle
            && !system_writer
            && !matches!(task.status, TaskStatus::Done | TaskStatus::Archived)
            && writer == TaskFieldWriter::Operator
        {
            self.human_crew(task.crew.as_deref())
                .map(|crew| crew.name.clone())
        } else {
            None
        };
        // The gates below see the bypass as the human override they already
        // implement; only the history entry tells the two apart.
        let gate_authority = if human_crew_bypass.is_some() {
            StatusAuthority::Forced
        } else {
            status_authority
        };
        if requested_status == Some(TaskStatus::Done)
            && gate_authority != StatusAuthority::Lifecycle
        {
            ensure_completion_run_stopped(
                self,
                &task,
                params.job_run_id.as_ref().and_then(|id| id.as_deref()),
                calling_run_id.as_deref(),
            )?;
            let human_force = gate_authority == StatusAuthority::Forced
                && operator_decision_authority
                && canonical_agent.is_none()
                && canonical_model.is_none()
                && !agent_context_declared();
            if !human_force {
                ensure_post_deploy_criteria_met(self, &task)?;
                self.ensure_mutation_proposals_applied(&task)?;
            }
        }
        // [granted-tools §4] An attributed completion is held the same way:
        // a task is never done while a mutation it proposed did not happen.
        if requested_status == Some(TaskStatus::Done)
            && gate_authority == StatusAuthority::Lifecycle
        {
            self.ensure_mutation_proposals_applied(&task)?;
        }
        let status_note = status_note
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(redact_all);
        if status_note.is_some() && requested_status.is_none() {
            return Err(OrbitError::InvalidInput(
                "`note` requires a status change; use `comment` for free-form discussion"
                    .to_string(),
            ));
        }
        if let Some(target) = requested_status
            && gate_authority == StatusAuthority::Lifecycle
        {
            ensure_status_change_allowed(self, &task, &params, target)?;
        }
        // Promotion to backlog validates a `verify-mock-ui` task's mocks
        // against the manifest at the base (visual-review §2.3), over the
        // tags and artifacts this update would leave stored.
        if requested_status == Some(TaskStatus::Backlog) && !system_writer {
            let mut effective = task.clone();
            if let Some(tags) = &params.tags {
                effective.tags = tags.clone();
            }
            if orbit_types::task::has_verify_mock_ui_tag(&effective.tags) {
                let mut artifacts = self.get_task_artifacts(id).unwrap_or_default();
                for upsert in &params.upsert_artifacts {
                    artifacts.retain(|stored| stored.path != upsert.path);
                    artifacts.push(upsert.clone());
                }
                self.ensure_backlog_mock_admission(&effective, Some(artifacts))?;
            }
        }
        if params.discard_candidate && task.status == TaskStatus::InProgress {
            return Err(OrbitError::InvalidInput(format!(
                "task {id} is in-progress; discard a preserved candidate after its run stops"
            )));
        }
        // Only the caller's own replacement counts: normalization may write
        // back a re-canonicalized stored list for an unrelated edit.
        let explicit_context_files = params.context_files.is_some();
        let validated = self.validate_and_normalize_task_field_edits(id, &task, params, writer)?;
        let crew_assignment = validated.crew_assignment;
        let crew_source = validated.crew_source;
        let crew_redraw_history = validated.crew_redraw_history;
        let granted_tools_change = validated.granted_tools_change;
        params = validated.params;
        if explicit_context_files
            && !system_writer
            && let (Some(owner), Some(requested)) =
                (artifact_owner.as_deref(), params.context_files.as_ref())
            && *requested != task.context_files
            && self.live_source_repair_owned_by(&task, owner)?
        {
            return Err(OrbitError::PolicyDenied(format!(
                "task {id} is being repaired by source-repair run `{owner}`, whose publication \
                 authority is bound to the admitted task scope, so this run cannot replace \
                 `context_files`; nothing in this update was written. Resend it without \
                 `context_files` (execution_summary and comment are accepted) and keep your \
                 changes in the worktree: the delivery commit widens `context_files` with the \
                 paths you actually changed."
            )));
        }
        // Operator work is completed by its operator: an agent-attributed
        // update cannot close a human crew's task, before or after a crew
        // change in the same write.
        if requested_status == Some(TaskStatus::Done)
            && status_authority == StatusAuthority::Lifecycle
            && writer == TaskFieldWriter::Agent
        {
            let next_crew = params
                .crew
                .as_ref()
                .map_or(task.crew.as_deref(), Option::as_deref);
            if let Some(human) = self
                .human_crew(task.crew.as_deref())
                .or_else(|| self.human_crew(next_crew))
            {
                return Err(OrbitError::PolicyDenied(format!(
                    "task {id} is assigned to human crew '{}': only an operator may move it to done",
                    human.name
                )));
            }
        }
        // The finding check above read the edits as supplied; judge the
        // material as it will be stored, since normalization can turn a
        // differently spelled tag into a review-finding tag.
        if let Some(target) = requested_status
            && gate_authority == StatusAuthority::Lifecycle
            && matches!(task.status, TaskStatus::Proposed | TaskStatus::Someday)
            && matches!(target, TaskStatus::Backlog | TaskStatus::InProgress)
        {
            self.ensure_promoted_finding_verified(&task, &params)?;
        }
        // A plain blocked/review-to-backlog move on an attributed surface applies
        // the same publication authentication as `orbit task requeue`:
        // refuse before writing what the next run would refuse, and record
        // the preservation a requeue would record for what it accepts.
        // Internal machinery and a discard keep their own contracts.
        let backlog_preservation = if requested_status == Some(TaskStatus::Backlog)
            && matches!(task.status, TaskStatus::Blocked | TaskStatus::Review)
            && matches!(
                status_authority,
                StatusAuthority::Lifecycle | StatusAuthority::Forced
            )
            && !system_writer
            && !params.discard_candidate
        {
            let mut preview = task.clone();
            if let Some(title) = &params.title {
                preview.title = title.clone();
            }
            if let Some(description) = &params.description {
                preview.description = description.clone();
            }
            if let Some(criteria) = &params.acceptance_criteria {
                preview.acceptance_criteria = criteria.clone();
            }
            if let Some(plan) = &params.plan {
                preview.plan = plan.clone();
            }
            if let Some(context_files) = &params.context_files {
                preview.context_files = context_files.clone();
            }
            if let Some(tags) = &params.tags {
                preview.tags = tags.clone();
            }
            if let Some(relations) = &params.relations {
                preview.relations = relations.clone();
            }
            if let Some(task_type) = params.task_type {
                preview.task_type = task_type;
            }
            if let Some(crew) = &params.crew {
                preview.crew = crew.clone();
            }
            // The write applies the provenance resolved with the crew, and the
            // publication digest covers it, so the preview carries it too.
            if let Some(source) = &crew_source {
                preview.crew_source = source.clone();
            }
            if let Some(complexity) = params.complexity {
                preview.complexity = Some(complexity);
            }
            self.authenticate_published_backlog_return(&preview)?
        } else {
            None
        };

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

        let operator_write = operator_decision_authority
            && canonical_agent.is_none()
            && canonical_model.is_none()
            && !super::super::helpers::is_automation_actor(&effective_label);
        // [ORB-14530] Review evidence counts only from a trusted writer class,
        // so it is stamped from the entry point, never from a label. A process
        // that declares an agent envelope or a managed run is never an operator.
        let artifact_writer = if system_writer {
            Some(ArtifactWriter::System)
        } else if operator_write && !agent_context_declared() {
            Some(ArtifactWriter::Operator)
        } else {
            None
        };
        let mut append_history: Vec<TaskHistoryEntry> = Vec::new();
        if let Some(preserved) = backlog_preservation {
            // The `requeued` decision is written only with its preservation
            // record, exactly as `write_candidate_requeue` writes it.
            if let Some(run_id) = task.job_run_id.clone() {
                append_history.push(TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: SYSTEM_ACTOR_LABEL.to_string(),
                    event: FINAL_RECOVERY_DECIDED_EVENT.to_string(),
                    note: Some(format!(
                        "operator_requeue (run_id={run_id}): outcome=requeued"
                    )),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: Some(TaskDecision::FinalRecovery {
                        run_id,
                        decision: "operator_requeue".to_string(),
                        outcome: RecordedFinalRecovery::Requeued,
                    }),
                });
            }
            append_history.push(preserved);
        }
        if operator_write
            && let Some(resolution) =
                self.pilot_hold_resolution(&task, &params, &effective_label)?
        {
            append_history.push(resolution);
        }
        if operator_write
            && let Some(result) =
                self.post_deploy_resolution(&task, params.comment.as_deref(), &effective_label)?
        {
            append_history.push(result);
        }
        if let Some(history) = crew_redraw_history {
            append_history.push(history);
        } else if let Some(replacement) = params.crew.as_ref()
            && (replacement.as_deref() != task.crew.as_deref()
                || crew_source
                    .as_ref()
                    .is_some_and(|source| *source != task.crew_source))
        {
            let source = match &crew_assignment {
                Some(assignment) if assignment.source == "explicit" => "explicit name".to_string(),
                Some(assignment) => format!("pool draw ({})", assignment.source),
                None if replacement.is_none() => "pool draw (no crew available)".to_string(),
                None => "explicit name".to_string(),
            };
            append_history.push(TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: effective_label.clone(),
                event: "crew_assigned".to_string(),
                note: Some(format!(
                    "crew changed from `{}` to `{}` via {source}",
                    describe_optional_field_value(task.crew.as_deref()),
                    describe_optional_field_value(replacement.as_deref()),
                )),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            });
        }
        if let (Some((before, state)), Some(after)) =
            (&granted_tools_change, params.granted_tools.as_deref())
        {
            // [granted-tools §1] Who changed the grant (`by`, the attributed
            // writer the chokepoint admitted as an operator), both tool sets,
            // and the run/claim state the change window admitted it in.
            append_history.push(TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: effective_label.clone(),
                event: GRANTED_TOOLS_CHANGED_EVENT.to_string(),
                note: Some(granted_tools_change_note(before, after, state)),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            });
        }
        if params.discard_candidate {
            // [ORB-13985] `candidate_resume` honours a discard recorded since
            // the candidate's run began, so the next run implements fresh.
            append_history.push(TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: effective_label.clone(),
                event: CANDIDATE_DISCARDED_EVENT.to_string(),
                note: Some(match task.job_run_id.as_deref() {
                    Some(run_id) => format!(
                        "discarded any candidate run `{run_id}` preserved; the next run implements fresh"
                    ),
                    None => "discarded any preserved candidate; the next run implements fresh"
                        .to_string(),
                }),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
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
                operation_id: None,
                decision: None,
            });
        }
        // A forced transition is still a transition: naming it in history is
        // what separates a human override from a governed lifecycle move.
        let status_event =
            if gate_authority == StatusAuthority::Forced && requested_status.is_some() {
                Some(FORCED_STATUS_EVENT.to_string())
            } else if guard_work_start
                && requested_status == Some(TaskStatus::InProgress)
                && !(operator_write && !agent_context_declared())
            {
                // An agent or managed-run shell moving a task into `in-progress`
                // starts work. Only a trusted operator's move (including the
                // rescue close of a blocked task) is exempt from the footprint
                // guard the store runs for a work-starting event.
                Some("started".to_string())
            } else {
                None
            };
        let evidence_attached = !params.upsert_artifacts.is_empty();
        let previous_status = task.status;
        let written_note = status_note.clone();
        // The forced entry names the human-crew bypass; the operator's own
        // note, optional, follows it.
        let status_note = match (&human_crew_bypass, status_note) {
            (Some(crew), Some(note)) => Some(format!("{HUMAN_CREW_BYPASS_NOTE} '{crew}': {note}")),
            (Some(crew), None) => Some(format!("{HUMAN_CREW_BYPASS_NOTE} '{crew}'")),
            (None, note) => note,
        };
        let mut updated = self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                id,
                TaskRecordUpdateParams {
                    artifact_owner_run_id: artifact_owner.clone(),
                    artifact_writer,
                    actor: effective_label.clone(),
                    crew_source: crew_source.clone(),
                    planned_by: attribution.planned_by.clone(),
                    implemented_by: attribution.implemented_by.clone(),
                    status_event: status_event.clone(),
                    status_note,
                    append_comments: append_comments.clone(),
                    append_history: append_history.clone(),
                    expected_status: expected_status.map(|status| vec![status]),
                    operator_comment: comment_only && operator_write && !agent_context_declared(),
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

        if evidence_attached {
            self.resume_review_evidence(id)?;
            updated = self.get_task(id)?;
        }
        if task.title != updated.title
            || task.description != updated.description
            || task.acceptance_criteria != updated.acceptance_criteria
        {
            self.schedule_task_quality_after_commit(&updated);
        }
        Ok(LockedTaskUpdate {
            task: updated,
            previous_status,
            status_note: written_note,
        })
    }
}

impl OrbitRuntime {
    /// Whether `owner` is the live source-repair run this task's history
    /// links to it [ORB-21019].
    ///
    /// Read under the task's write lock from the run store and the system
    /// `source_repair_linked` history entry; request input and actor labels
    /// contribute nothing. A run that is terminal, carries no source-repair
    /// admission for this task, or was never linked by the system is not one.
    fn live_source_repair_owned_by(&self, task: &Task, owner: &str) -> Result<bool, OrbitError> {
        let Some(run) = self.stored_job_run(owner)? else {
            return Ok(false);
        };
        if run.state.is_terminal() {
            return Ok(false);
        }
        let Some(admission) = run
            .input
            .as_ref()
            .and_then(|input| input.get(SOURCE_REPAIR_KEY))
        else {
            return Ok(false);
        };
        let Some(request_key) = admission.get("request_key").and_then(Value::as_str) else {
            return Ok(false);
        };
        if admission["binding"]["task_id"].as_str() != Some(task.id.as_str()) {
            return Ok(false);
        }
        Ok(self.get_task_history(&task.id)?.iter().any(|entry| {
            entry.event == SOURCE_REPAIR_LINKED
                && entry.by == SYSTEM_ACTOR_LABEL
                && entry
                    .note
                    .as_deref()
                    .and_then(|note| serde_json::from_str::<Value>(note).ok())
                    .is_some_and(|link| {
                        link["run_id"].as_str() == Some(owner)
                            && link["request_key"].as_str() == Some(request_key)
                    })
        }))
    }
}
