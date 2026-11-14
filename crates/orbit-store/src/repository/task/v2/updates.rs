use super::*;
use crate::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use crate::driver::file::task_bundle::PendingWriteGuard;
use orbit_types::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, CrewSource, DESCRIPTION_DOCUMENT, EXECUTION_SUMMARY_DOCUMENT,
    PLAN_DOCUMENT, TAGS_CHANGED_EVENT, TASK_FIELDS_UPDATED_EVENT, TagsChanged, TaskDecision,
    named_friction_targets, pending_resolves_entries, task_fields_updated_note,
};

/// The `tags_changed` event a write appends when it leaves different tags
/// than it found, carrying both lists so a reader can replay the tags in
/// force at any earlier time.
fn tags_changed_event(
    bundle: &TaskBundleV2,
    actor: &str,
    at: chrono::DateTime<Utc>,
    previous: Vec<String>,
    tags: Vec<String>,
) -> TaskEventRowV2 {
    let render = |list: &[String]| {
        if list.is_empty() {
            "(none)".to_string()
        } else {
            list.join(", ")
        }
    };
    TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id: next_event_id(&bundle.events),
        at,
        by: actor.to_string(),
        event_type: TAGS_CHANGED_EVENT.to_string(),
        note: Some(format!(
            "tags changed: {} → {}",
            render(&previous),
            render(&tags)
        )),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: Some(TaskDecision::TagsChanged(TagsChanged { previous, tags })),
    }
}

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
            || fields.history_summary.trim().is_empty()
            || fields.history_summary.chars().any(char::is_control)
            || fields.audit_note.trim().is_empty()
        {
            return Err(OrbitError::InvalidInput(
                "atomic task mutation requires actor, operation id, event type, a single-line history summary, and audit note"
                    .to_string(),
            ));
        }

        reject_forged_grant(&fields.append_history)?;
        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            if bundle.events.iter().any(|event| {
                event.event_type == fields.event_type
                    && event.receipt() == Some(fields.operation_id.as_str())
            }) {
                return Ok(AtomicTaskMutationOutcome::AlreadyApplied);
            }
            if bundle.envelope.context_files != fields.expected_context_files
                || bundle.envelope.status != fields.expected_status
                || bundle.envelope.complexity != fields.expected_complexity
                || bundle.envelope.crew != fields.expected_crew
                || bundle.envelope.crew_source != fields.expected_crew_source
                || creation_state(&bundle).identity() != fields.expected_context_creation
            {
                return Ok(AtomicTaskMutationOutcome::Stale);
            }

            let tags = (!fields.add_tags.is_empty())
                .then(|| {
                    let mut tags = bundle.envelope.tags.clone();
                    tags.extend(fields.add_tags.iter().cloned());
                    let tags = normalize_task_tags(tags);
                    validate_os_tags(&tags).map(|()| tags)
                })
                .transpose()?;

            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let mut pending_events: Vec<TaskEventRowV2> = Vec::new();
            let mut pending_comments: Vec<TaskCommentRowV2> = Vec::new();
            let now = Utc::now();
            for entry in &fields.append_history {
                let event = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: next_event_id(&bundle.events),
                    at: entry.at,
                    by: fields.actor.clone(),
                    event_type: entry.event.clone(),
                    note: entry.note.clone(),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: entry.decision.clone(),
                };
                pending_events.push(event.clone());
                bundle.events.push(event);
            }
            let status_changed = fields.status != bundle.envelope.status;
            let event = TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: next_event_id(&bundle.events),
                at: now,
                by: fields.actor.clone(),
                event_type: fields.event_type.clone(),
                note: Some(fields.history_summary.clone()),
                from_status: status_changed.then_some(bundle.envelope.status),
                to_status: status_changed.then_some(fields.status),
                operation_id: Some(fields.operation_id.clone()),
                decision: fields.decision.clone(),
            };
            pending_events.push(event.clone());
            bundle.events.push(event);
            let comment = TaskCommentRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                comment_id: format!("C-{:04}", next_sequence(&bundle.comments, "C-")),
                at: now,
                by: fields.actor.clone(),
                body: fields.audit_note.clone(),
                operation_id: Some(fields.operation_id.clone()),
            };
            pending_comments.push(comment);
            append_creation_grant(
                &mut bundle,
                &fields.context_files,
                &[],
                &fields.actor,
                now,
                &mut pending_events,
            )?;
            bundle.envelope.context_files = fields.context_files.clone();
            for target in &fields.add_dependencies {
                let relation = TaskRelation {
                    relation_type: TaskRelationType::BlockedBy,
                    target: target.clone(),
                    regression: None,
                };
                if !bundle.envelope.relations.contains(&relation) {
                    bundle.envelope.relations.push(relation);
                }
            }
            if let Some(tags) = tags {
                if tags != bundle.envelope.tags {
                    let event = tags_changed_event(
                        &bundle,
                        &fields.actor,
                        now,
                        bundle.envelope.tags.clone(),
                        tags.clone(),
                    );
                    pending_events.push(event.clone());
                    bundle.events.push(event);
                }
                bundle.envelope.tags = tags;
            }
            self.bundle_store.append_events(id, &pending_events)?;
            self.bundle_store.append_comments(id, &pending_comments)?;
            bundle.envelope.status = fields.status;
            bundle.envelope.complexity = Some(fields.complexity);
            bundle.envelope.crew = fields.crew.clone();
            bundle.envelope.crew_source = fields.crew_source.clone();
            bundle.envelope.updated_at = now;
            self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
            pending.finish();
            self.publish_task_index_and_lease_notices(&bundle.envelope, &fields.event_note)?;
            Ok(AtomicTaskMutationOutcome::Applied)
        })
    }

    /// Rewrite the task's stored pre-family tool names (`required_tools`,
    /// `granted_tools`) to family actions [ORB-23552], recording one
    /// `tool_names_migrated` history event. The rewrite names the same
    /// operations, so it is a migration rather than an edit: `updated_at`, the
    /// creation grant and every other field stay as they were. Returns whether
    /// anything changed; a task already holding family actions is untouched.
    pub(crate) fn migrate_task_tool_names(
        &self,
        id: &str,
        actor: &str,
    ) -> Result<bool, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }
        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            let required =
                orbit_types::tool::family_action_tool_names(&bundle.envelope.required_tools);
            let granted =
                orbit_types::tool::family_action_tool_names(&bundle.envelope.granted_tools);
            if required.is_none() && granted.is_none() {
                return Ok(false);
            }
            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let mut changes = Vec::new();
            if let Some(after) = &required {
                changes.push((
                    "required_tools",
                    bundle.envelope.required_tools.as_slice(),
                    after.as_slice(),
                ));
            }
            if let Some(after) = &granted {
                changes.push((
                    "granted_tools",
                    bundle.envelope.granted_tools.as_slice(),
                    after.as_slice(),
                ));
            }
            let event = TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: next_event_id(&bundle.events),
                at: Utc::now(),
                by: actor.to_string(),
                event_type: orbit_types::task::TOOL_NAMES_MIGRATED_EVENT.to_string(),
                note: Some(orbit_types::task::tool_names_migrated_note(&changes)),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            };
            self.bundle_store
                .append_events(id, std::slice::from_ref(&event))?;
            bundle.events.push(event);
            if let Some(after) = required {
                bundle.envelope.required_tools = after;
            }
            if let Some(after) = granted {
                bundle.envelope.granted_tools = after;
            }
            self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
            pending.finish();
            self.publish_task_index_and_lease_notices(
                &bundle.envelope,
                "task tool names migration",
            )?;
            Ok(true)
        })
    }

    pub(crate) fn update_task_document(
        &self,
        id: &str,
        fields: &TaskDocumentUpdateParams,
    ) -> Result<crate::contracts::TaskRow, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }
        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            let original_envelope = bundle.envelope.clone();
            let original_description = bundle.description.clone();
            let original_acceptance = bundle.acceptance.clone();
            let original_plan = bundle.plan.clone();
            let original_execution_summary = bundle.execution_summary.clone();
            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let mut pending_events: Vec<TaskEventRowV2> = Vec::new();
            let updated_at = Utc::now();
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
                let tags = normalize_task_tags(value.clone());
                validate_os_tags(&tags)?;
                bundle.envelope.tags = tags;
                envelope_changed = true;
            }
            if let Some(value) = &fields.granted_tools {
                bundle.envelope.granted_tools =
                    orbit_types::task::normalize_required_tools(value.clone());
                envelope_changed = true;
            }
            if let Some(value) = &fields.requires_labels {
                bundle.envelope.requires_labels = orbit_types::task::normalize_host_labels(
                    value.clone(),
                    orbit_types::task::REQUIRES_LABELS_FIELD,
                )?;
                envelope_changed = true;
            }
            if let Some(value) = fields.not_before {
                bundle.envelope.not_before = value;
                envelope_changed = true;
            }
            if let Some(value) = &fields.context_files {
                append_creation_grant(
                    &mut bundle,
                    value,
                    &fields.context_creation,
                    &fields.actor,
                    updated_at,
                    &mut pending_events,
                )?;
                bundle.envelope.context_files = value.clone();
                envelope_changed = true;
            } else if !fields.context_creation.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "creation authorization requires the context_files it applies to".to_string(),
                ));
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
                        regression: None,
                    }),
                );
                envelope_changed = true;
            }
            if let Some(value) = &fields.source_task_id {
                // Re-setting the same culprit keeps the link's classification,
                // including its recorded sites.
                let replacements = value
                    .iter()
                    .cloned()
                    .map(|target| {
                        let regression = bundle
                            .envelope
                            .relations
                            .iter()
                            .find(|relation| {
                                relation.relation_type == TaskRelationType::RegressionFrom
                                    && relation.target == target
                            })
                            .and_then(|relation| relation.regression.clone());
                        TaskRelation {
                            relation_type: TaskRelationType::RegressionFrom,
                            target,
                            regression,
                        }
                    })
                    .collect::<Vec<_>>();
                replace_relations(
                    &mut bundle.envelope.relations,
                    TaskRelationType::RegressionFrom,
                    replacements,
                );
                envelope_changed = true;
            }
            if let Some(value) = &fields.relations {
                bundle.envelope.relations = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.job_run_id {
                if value != &bundle.envelope.job_run_id {
                    bundle.envelope.job_run_machine = fields.job_run_machine.clone().flatten();
                } else if let Some(location) = &fields.job_run_machine
                    && location != &bundle.envelope.job_run_machine
                {
                    return Err(OrbitError::InvalidInput(
                        "execution location is immutable for a run binding".into(),
                    ));
                }
                if value.is_none() {
                    bundle.envelope.job_run_machine = None;
                }
                bundle.envelope.job_run_id = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.crew {
                bundle.envelope.crew = value.clone();
                // A crew replacement without provenance is an explicit pin.
                bundle.envelope.crew_source = value.as_ref().map(|_| CrewSource::Explicit);
                envelope_changed = true;
            }
            if let Some(value) = &fields.crew_source {
                bundle.envelope.crew_source = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = &fields.orchestrator {
                bundle.envelope.orchestrator = value.clone();
                envelope_changed = true;
            }
            if let Some(value) = fields.origin {
                bundle.envelope.origin = Some(value);
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

            // Body documents are separate files. The post-write row is this
            // in-memory bundle, so it has to carry the same bytes just published
            // or a caller would observe the pre-write description, criteria,
            // plan or execution summary.
            if let Some(value) = &fields.description {
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::Description, value)?;
                bundle.description = value.clone();
            }
            if let Some(value) = &fields.acceptance_criteria {
                let rendered = render_acceptance(value);
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::Acceptance, &rendered)?;
                bundle.acceptance = rendered;
            }
            if let Some(value) = &fields.plan {
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::Plan, value)?;
                bundle.plan = value.clone();
            }
            if let Some(value) = &fields.execution_summary {
                self.bundle_store
                    .rewrite_document(id, TaskDocumentV2::ExecutionSummary, value)?;
                bundle.execution_summary = value.clone();
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
                    operation_id: None,
                    decision: None,
                };
                pending_events.push(event.clone());
                bundle.events.push(event);
            }

            if bundle.envelope.tags != original_envelope.tags {
                let event = tags_changed_event(
                    &bundle,
                    &fields.actor,
                    Utc::now(),
                    original_envelope.tags.clone(),
                    bundle.envelope.tags.clone(),
                );
                pending_events.push(event.clone());
                bundle.events.push(event);
            }

            // Drain approval snapshots use task history to distinguish a real
            // operator edit from the creation-grant row that merely re-seals
            // unchanged intent. Record content and scope changes alongside
            // the document write so the grant row cannot hide them.
            let mut prior_unrecorded_envelope = original_envelope.clone();
            prior_unrecorded_envelope.title = bundle.envelope.title.clone();
            prior_unrecorded_envelope.crew = bundle.envelope.crew.clone();
            prior_unrecorded_envelope.crew_source = bundle.envelope.crew_source.clone();
            if fields.source_task_id.is_some() && fields.relations.is_none() {
                prior_unrecorded_envelope
                    .relations
                    .retain(|relation| relation.relation_type != TaskRelationType::RegressionFrom);
                prior_unrecorded_envelope.relations.extend(
                    bundle
                        .envelope
                        .relations
                        .iter()
                        .filter(|relation| {
                            relation.relation_type == TaskRelationType::RegressionFrom
                        })
                        .cloned(),
                );
            }
            let envelope_fields_changed = prior_unrecorded_envelope != bundle.envelope;
            // The note names each changed document, so an edit to what the
            // task asks reads apart from run bookkeeping.
            let changed_documents: Vec<&str> = [
                (
                    DESCRIPTION_DOCUMENT,
                    fields
                        .description
                        .as_ref()
                        .is_some_and(|value| value != &original_description),
                ),
                (
                    ACCEPTANCE_CRITERIA_DOCUMENT,
                    fields
                        .acceptance_criteria
                        .as_ref()
                        .is_some_and(|value| render_acceptance(value) != original_acceptance),
                ),
                (
                    PLAN_DOCUMENT,
                    fields
                        .plan
                        .as_ref()
                        .is_some_and(|value| value != &original_plan),
                ),
                (
                    EXECUTION_SUMMARY_DOCUMENT,
                    fields
                        .execution_summary
                        .as_ref()
                        .is_some_and(|value| value != &original_execution_summary),
                ),
            ]
            .into_iter()
            .filter_map(|(document, changed)| changed.then_some(document))
            .collect();
            let context_creation_changed = !fields.context_creation.is_empty();
            if envelope_fields_changed || !changed_documents.is_empty() || context_creation_changed
            {
                let event = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: next_event_id(&bundle.events),
                    at: Utc::now(),
                    by: fields.actor.clone(),
                    event_type: TASK_FIELDS_UPDATED_EVENT.to_string(),
                    note: Some(task_fields_updated_note(&changed_documents)),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                };
                pending_events.push(event.clone());
                bundle.events.push(event);
            }

            if envelope_changed
                || fields.description.is_some()
                || fields.acceptance_criteria.is_some()
                || fields.plan.is_some()
                || fields.execution_summary.is_some()
            {
                if fields.context_files.is_none() {
                    let context_files = bundle.envelope.context_files.clone();
                    append_creation_grant(
                        &mut bundle,
                        &context_files,
                        &[],
                        &fields.actor,
                        updated_at,
                        &mut pending_events,
                    )?;
                }
                self.bundle_store.append_events(id, &pending_events)?;
                bundle.envelope.updated_at = updated_at;
                self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
                pending.finish();
                self.publish_task_index_and_lease_notices(
                    &bundle.envelope,
                    "task document update",
                )?;
            } else {
                self.bundle_store.append_events(id, &pending_events)?;
                pending.finish();
            }
            // The in-memory bundle is the post-write row. Callers answer from
            // it instead of reading the bundle they just published.
            self.row_from_bundle(bundle)
        })
    }

    /// Pre-flight form of the work-start guard in [`Self::update_task_history`]:
    /// same target/event classification and same claim check, evaluated
    /// against the footprint the combined write would start work on.
    pub(crate) fn check_ordinary_work_start_footprint(
        &self,
        id: &str,
        status: Option<TaskStatus>,
        status_event: Option<&str>,
        context_files: Option<&[String]>,
    ) -> Result<(), OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if !matches!(
            status_event,
            Some("started" | "pulled_by" | "resume_readmitted")
        ) {
            return Ok(());
        }
        self.with_task_read_lock(id, || {
            let bundle = self.read_existing_bundle(id)?;
            let target_status = status.unwrap_or(bundle.envelope.status);
            if target_status != TaskStatus::InProgress {
                return Ok(());
            }
            let effective_files: Vec<String> = match context_files {
                Some(files) => files.to_vec(),
                None => bundle.envelope.context_files.clone(),
            };
            if let Some(boundary) = &self.coordination {
                boundary.guard_ordinary_footprint(target_status, &effective_files)?;
            }
            Ok(())
        })
    }

    pub(crate) fn update_task_history(
        &self,
        id: &str,
        fields: &TaskHistoryUpdateParams,
    ) -> Result<crate::contracts::TaskRow, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }

        reject_forged_grant(&fields.append_history)?;
        // [ORB-16723] An operator's comment is the one unscoped write an
        // active execution claim admits: its only effect is appending one
        // comment to the append-only log, where the claim's steering reads it.
        // Any other effect keeps the ordinary fence and its refusal.
        let comment_only = fields.operator_comment
            && fields.append_comments.len() == 1
            && fields.status.is_none()
            && fields.status_event.is_none()
            && fields.status_note.is_none()
            && fields.status_decision.is_none()
            && fields.append_history.is_empty()
            && fields.expected_status.is_none()
            && fields.planned_by.is_none()
            && fields.implemented_by.is_none();
        let write = |claimed: bool| {
            let mut bundle = self.read_existing_bundle(id)?;
            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let mut pending_events: Vec<TaskEventRowV2> = Vec::new();
            let mut pending_comments: Vec<TaskCommentRowV2> = Vec::new();
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
            let work_starting = target_status == TaskStatus::InProgress
                && matches!(
                    fields.status_event.as_deref(),
                    Some("started" | "pulled_by" | "resume_readmitted")
                );
            if work_starting && let Some(boundary) = &self.coordination {
                boundary.guard_ordinary_footprint(target_status, &bundle.envelope.context_files)?;
            }
            let status_transition =
                (target_status != current_status).then_some((current_status, target_status));
            if fields.planned_by.is_some() || fields.implemented_by.is_some() {
                let previous = bundle.envelope.clone();
                if let Some(value) = &fields.planned_by {
                    bundle.envelope.planned_by = value.clone();
                }
                if let Some(value) = &fields.implemented_by {
                    bundle.envelope.implemented_by = value.clone();
                }
                if bundle.envelope != previous {
                    let event = TaskEventRowV2 {
                        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                        event_id: next_event_id(&bundle.events),
                        at: now,
                        by: fields.actor.clone(),
                        event_type: TASK_FIELDS_UPDATED_EVENT.to_string(),
                        note: Some(task_fields_updated_note(&[])),
                        from_status: None,
                        to_status: None,
                        operation_id: None,
                        decision: None,
                    };
                    pending_events.push(event.clone());
                    bundle.events.push(event);
                }
                // The document write this replaces sealed any due grant here,
                // ahead of the status event; keep that history order.
                if !claimed {
                    let context_files = bundle.envelope.context_files.clone();
                    append_creation_grant(
                        &mut bundle,
                        &context_files,
                        &[],
                        &fields.actor,
                        now,
                        &mut pending_events,
                    )?;
                }
            }
            let mut next_event = next_sequence(&bundle.events, "EV-");
            let mut next_comment = next_sequence(&bundle.comments, "C-");
            // Retry work commits ahead of the transition into done, so no
            // crash or later read failure can leave done without it.
            let pending_resolves =
                if status_transition.is_some_and(|(_, to)| to == TaskStatus::Done) {
                    pending_resolves_entries(
                        &named_friction_targets(
                            &bundle.envelope.relations,
                            [bundle.description.as_str(), bundle.acceptance.as_str()],
                        ),
                        &fields.actor,
                        now,
                    )
                } else {
                    Vec::new()
                };

            for entry in fields.append_history.iter().chain(&pending_resolves) {
                let event = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: format!("EV-{next_event:04}"),
                    at: entry.at,
                    by: entry.by.clone(),
                    event_type: entry.event.clone(),
                    note: entry.note.clone(),
                    from_status: entry.from_status,
                    to_status: entry.to_status,
                    operation_id: None,
                    decision: entry.decision.clone(),
                };
                pending_events.push(event.clone());
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
                    operation_id: None,
                };
                pending_comments.push(row.clone());
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
                    operation_id: None,
                    decision: fields.status_decision.clone(),
                };
                pending_events.push(event.clone());
                bundle.events.push(event);
            }

            if !fields.append_history.is_empty()
                || !fields.append_comments.is_empty()
                || fields.status.is_some()
                || fields.status_event.is_some()
                || fields.status_note.is_some()
            {
                // A claimed task's creation grant is the claim's to keep: a
                // comment admitted under it records no new grant.
                if !claimed {
                    let context_files = bundle.envelope.context_files.clone();
                    append_creation_grant(
                        &mut bundle,
                        &context_files,
                        &[],
                        &fields.actor,
                        now,
                        &mut pending_events,
                    )?;
                }
                self.bundle_store.append_events(id, &pending_events)?;
                self.bundle_store.append_comments(id, &pending_comments)?;
                bundle.envelope.status = target_status;
                bundle.envelope.updated_at = now;
                self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
                pending.finish();
                self.publish_task_index_and_lease_notices(&bundle.envelope, "task history update")?;
            } else {
                self.bundle_store.append_events(id, &pending_events)?;
                self.bundle_store.append_comments(id, &pending_comments)?;
                pending.finish();
            }
            // Same post-write row as a document update: history runs after
            // the document commit, so this bundle includes both.
            self.row_from_bundle(bundle)
        };
        if comment_only {
            self.with_task_comment_lock(id, write)
        } else {
            self.with_task_lock(id, || write(false))
        }
    }
}
