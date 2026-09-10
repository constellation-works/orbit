use super::*;
use crate::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use crate::driver::file::task_bundle::{BundleWriteFault, PendingWriteGuard, fail_if_injected};

impl TaskV2Store {
    pub(crate) fn apply_atomic_task_mutation(
        &self,
        id: &str,
        fields: &AtomicTaskMutationParams,
    ) -> Result<AtomicTaskMutationOutcome, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.actor.trim().is_empty()
            || fields.operation_id.trim().is_empty()
            || fields.event_type.trim().is_empty()
        {
            return Err(OrbitError::InvalidInput(
                "atomic task mutation actor, operation id, and event type must not be empty"
                    .to_string(),
            ));
        }

        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            let receipt = format!("operation_id={}", fields.operation_id);
            if bundle.events.iter().any(|event| {
                event.event_type == fields.event_type
                    && event.note.as_deref() == Some(receipt.as_str())
            }) {
                return Ok(AtomicTaskMutationOutcome::AlreadyApplied);
            }
            if bundle.envelope.context_files != fields.expected_context_files
                || bundle.envelope.status != fields.expected_status
            {
                return Ok(AtomicTaskMutationOutcome::Stale);
            }

            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let now = Utc::now();
            let status_changed = fields.status != bundle.envelope.status;
            let event = TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: next_event_id(&bundle.events),
                at: now,
                by: fields.actor.clone(),
                event_type: fields.event_type.clone(),
                note: Some(receipt),
                from_status: status_changed.then_some(bundle.envelope.status),
                to_status: status_changed.then_some(fields.status),
            };
            self.bundle_store.append_event(id, &event)?;
            fail_if_injected(BundleWriteFault::AfterJsonlAppend)?;

            bundle.envelope.context_files = fields.context_files.clone();
            bundle.envelope.status = fields.status;
            bundle.envelope.updated_at = now;
            self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
            pending.finish();
            self.replace_index_best_effort(&bundle.envelope, &fields.event_note);
            Ok(AtomicTaskMutationOutcome::Applied)
        })
    }

    pub(crate) fn update_task_document(
        &self,
        id: &str,
        fields: &TaskDocumentUpdateParams,
    ) -> Result<(), OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }
        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let mut envelope_changed = false;
            let mut title_changed = false;
            let mut previous_title: Option<String> = None;
            let relations_changed = fields.dependencies.is_some()
                || fields.source_task_id.is_some()
                || fields.relations.is_some();

            if let Some(value) = &fields.title {
                if value.trim().is_empty() {
                    return Err(OrbitError::InvalidInput(
                        "task title must not be empty".to_string(),
                    ));
                }
                title_changed = *value != bundle.envelope.title;
                if title_changed {
                    previous_title = Some(bundle.envelope.title.clone());
                }
                bundle.envelope.title = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.tags {
                bundle.envelope.tags = normalize_task_tags(value.clone());
                envelope_changed = true;
            }
            if let Some(value) = &fields.context_files {
                bundle.envelope.context_files = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.created_by {
                bundle.envelope.created_by = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.planned_by {
                bundle.envelope.planned_by = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.implemented_by {
                bundle.envelope.implemented_by = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = fields.priority {
                bundle.envelope.priority = value;
                envelope_changed = true;
            }
            if let Some(value) = fields.complexity {
                bundle.envelope.complexity = Some(value);
                envelope_changed = true;
            }
            if let Some(value) = fields.task_type {
                bundle.envelope.task_type = value;
                envelope_changed = true;
            }
            if let Some(value) = &fields.pr_status {
                bundle.envelope.pr_status = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.dependencies {
                replace_relations(
                    &mut bundle.envelope.relations,
                    TaskRelationType::BlockedBy,
                    value.iter().cloned().map(|target| TaskRelation {
                        relation_type: TaskRelationType::BlockedBy,
                        target,
                    }),
                );
                envelope_changed = true;
            }
            if let Some(value) = &fields.source_task_id {
                replace_relations(
                    &mut bundle.envelope.relations,
                    TaskRelationType::RegressionFrom,
                    value.iter().cloned().map(|target| TaskRelation {
                        relation_type: TaskRelationType::RegressionFrom,
                        target,
                    }),
                );
                envelope_changed = true;
            }
            if let Some(value) = &fields.relations {
                bundle.envelope.relations = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.job_run_id {
                bundle.envelope.job_run_id = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.crew {
                bundle.envelope.crew = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.orchestrator {
                bundle.envelope.orchestrator = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.external_refs {
                bundle.envelope.external_refs = value.clone();
                envelope_changed = true;
            }

            if relations_changed {
                self.registry.validate_task_relations(
                    &self.workspace_id,
                    id,
                    &bundle.envelope.relations,
                )?;
            }

            if let Some(value) = &fields.description {
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::Description, value)?;
            }
            if let Some(value) = &fields.acceptance_criteria {
                self.bundle_store.rewrite_document(
                    id,
                    TaskDocumentV2::Acceptance,
                    &render_acceptance(value),
                )?;
            }
            if let Some(value) = &fields.plan {
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::Plan, value)?;
            }
            if let Some(value) = &fields.execution_summary {
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::ExecutionSummary, value)?;
            }

            if title_changed {
                let now = Utc::now();
                // ORB-10311: capture both titles so the rename is auditable from
                // history alone rather than an empty `renamed` marker.
                let note = previous_title
                    .as_ref()
                    .map(|previous| format!("\"{previous}\" → \"{}\"", bundle.envelope.title));
                let event = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: next_event_id(&bundle.events),
                    at: now,
                    by: fields.actor.clone(),
                    event_type: "renamed".to_string(),
                    note,
                    from_status: None,
                    to_status: None,
                };
                self.bundle_store.append_event(id, &event)?;
                bundle.events.push(event);
            }

            fail_if_injected(BundleWriteFault::AfterJsonlAppend)?;
            if envelope_changed
                || fields.description.is_some()
                || fields.acceptance_criteria.is_some()
                || fields.plan.is_some()
                || fields.execution_summary.is_some()
            {
                bundle.envelope.updated_at = Utc::now();
                self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
                pending.finish();
                self.replace_index_best_effort(&bundle.envelope, "task document update");
            } else {
                pending.finish();
            }
            Ok(())
        })
    }

    pub(crate) fn update_task_history(
        &self,
        id: &str,
        fields: &TaskHistoryUpdateParams,
    ) -> Result<(), OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }

        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let now = Utc::now();
            let current_status = bundle.envelope.status;
            // [ORB-11305] Compare-and-set, evaluated against the bundle we just
            // re-read under the exclusive lock. A caller that decided this write
            // was allowed from an earlier read loses here rather than silently
            // overwriting whatever landed in between.
            if let Some(expected) = &fields.expected_status
                && !expected.contains(&current_status)
            {
                pending.finish();
                return Err(OrbitError::InvalidInput(format!(
                    "task '{id}' status changed to '{current_status}' before this write; \
                     expected one of [{}]",
                    expected
                        .iter()
                        .map(TaskStatus::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
            let target_status = fields.status.unwrap_or(current_status);
            let status_transition =
                (target_status != current_status).then_some((current_status, target_status));
            let mut next_event = next_sequence(&bundle.events, "EV-");
            let mut next_comment = next_sequence(&bundle.comments, "C-");

            for entry in &fields.append_history {
                let event = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: format!("EV-{next_event:04}"),
                    at: entry.at,
                    by: entry.by.clone(),
                    event_type: entry.event.clone(),
                    note: entry.note.clone(),
                    from_status: entry.from_status,
                    to_status: entry.to_status,
                };
                self.bundle_store.append_event(id, &event)?;
                bundle.events.push(event);
                next_event += 1;
            }

            for comment in &fields.append_comments {
                let row = TaskCommentRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    comment_id: format!("C-{next_comment:04}"),
                    at: comment.at,
                    by: comment.by.clone(),
                    body: comment.message.clone(),
                };
                self.bundle_store.append_comment(id, &row)?;
                bundle.comments.push(row);
                next_comment += 1;
            }

            let event_type = fields
                .status_event
                .clone()
                .or_else(|| status_transition.map(|_| "status_changed".to_string()));
            if let Some(event_type) = event_type {
                let event = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: format!("EV-{next_event:04}"),
                    at: now,
                    by: fields.actor.clone(),
                    event_type,
                    note: fields.status_note.clone(),
                    from_status: status_transition.map(|(from, _)| from),
                    to_status: status_transition.map(|(_, to)| to),
                };
                self.bundle_store.append_event(id, &event)?;
                bundle.events.push(event);
            }

            fail_if_injected(BundleWriteFault::AfterJsonlAppend)?;
            if !fields.append_history.is_empty()
                || !fields.append_comments.is_empty()
                || fields.status.is_some()
                || fields.status_event.is_some()
                || fields.status_note.is_some()
            {
                bundle.envelope.status = target_status;
                bundle.envelope.updated_at = now;
                self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
                pending.finish();
                self.replace_index_best_effort(&bundle.envelope, "task history update");
            } else {
                pending.finish();
            }
            Ok(())
        })
    }
}
