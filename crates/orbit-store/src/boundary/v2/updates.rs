use super::*;
use crate::contract::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use crate::driver::file::task_bundle::PendingWriteGuard;
use orbit_types::task::{
    ACCEPTANCE_CRITERIA_DOCUMENT, CrewSource, DESCRIPTION_DOCUMENT, EXECUTION_SUMMARY_DOCUMENT,
    PLAN_DOCUMENT, TAGS_CHANGED_EVENT, TASK_FIELDS_UPDATED_EVENT, TagsChanged, TaskDecision,
    named_friction_targets, pending_resolves_entries, task_fields_updated_note,
};

impl TaskV2Store {
    pub fn apply_atomic_task_mutation(
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
                    decision: entry.decision.clone(),
                    ..event_row(
                        next_event_id(&bundle.events),
                        entry.at,
                        &fields.actor,
                        entry.event.clone(),
                        entry.note.clone(),
                    )
                };
                record_event(&mut bundle, &mut pending_events, event);
            }
            let status_changed = fields.status != bundle.envelope.status;
            let event = TaskEventRowV2 {
                from_status: status_changed.then_some(bundle.envelope.status),
                to_status: status_changed.then_some(fields.status),
                operation_id: Some(fields.operation_id.clone()),
                decision: fields.decision.clone(),
                ..event_row(
                    next_event_id(&bundle.events),
                    now,
                    &fields.actor,
                    fields.event_type.clone(),
                    Some(fields.history_summary.clone()),
                )
            };
            record_event(&mut bundle, &mut pending_events, event);
            let comment = TaskCommentRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                comment_id: format!("C-{:04}", next_sequence(&bundle.comments, "C-")),
                at: now,
                by: fields.actor.clone(),
                body: fields.audit_note.clone(),
                operation_id: Some(fields.operation_id.clone()),
            };
            pending_comments.push(comment);
            let creation_sealed_at = append_creation_grant(
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
                    record_event(&mut bundle, &mut pending_events, event);
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
            bundle.envelope.context_creation_sealed_at = creation_sealed_at;
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
            let event = event_row(
                next_event_id(&bundle.events),
                Utc::now(),
                actor,
                orbit_types::task::TOOL_NAMES_MIGRATED_EVENT,
                Some(orbit_types::task::tool_names_migrated_note(&changes)),
            );
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
    ) -> Result<crate::contract::TaskRow, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }
        self.with_task_lock(id, || {
            let mut bundle = self.read_existing_bundle(id)?;
            let originals = DocumentOriginals::capture(&bundle);
            let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
            let mut pending_events: Vec<TaskEventRowV2> = Vec::new();
            let updated_at = Utc::now();
            let edits =
                apply_envelope_fields(&mut bundle, fields, updated_at, &mut pending_events)?;
            if fields.dependencies.is_some()
                || fields.source_task_id.is_some()
                || fields.relations.is_some()
            {
                self.registry.validate_task_relations(
                    &self.workspace_id,
                    id,
                    &bundle.envelope.relations,
                )?;
            }
            self.write_document_bodies(id, &mut bundle, fields)?;
            record_document_events(
                &mut bundle,
                fields,
                &originals,
                edits.previous_title,
                &mut pending_events,
            );

            let publishes_envelope = edits.changed
                || fields.description.is_some()
                || fields.acceptance_criteria.is_some()
                || fields.plan.is_some()
                || fields.execution_summary.is_some();
            let sealed_at = if publishes_envelope {
                Some(match edits.creation_sealed_at {
                    Some(sealed_at) => sealed_at,
                    None => {
                        let context_files = bundle.envelope.context_files.clone();
                        append_creation_grant(
                            &mut bundle,
                            &context_files,
                            &[],
                            &fields.actor,
                            updated_at,
                            &mut pending_events,
                        )?
                    }
                })
            } else {
                None
            };
            stamp_pending_events(
                &mut bundle.events,
                &mut pending_events,
                fields.write_provenance.as_ref(),
            );
            self.bundle_store.append_events(id, &pending_events)?;
            if let Some(sealed_at) = sealed_at {
                bundle.envelope.updated_at = updated_at;
                bundle.envelope.context_creation_sealed_at = sealed_at;
                self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
                pending.finish();
                self.publish_task_index_and_lease_notices(
                    &bundle.envelope,
                    "task document update",
                )?;
            } else {
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
        if !starts_ordinary_work(status_event) {
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

    /// One history write: its events, its comments and any envelope change.
    ///
    /// A comment with a nonblank `operation_id` replays as a no-op when this
    /// task's own comment log already holds that ID, including an earlier
    /// comment of the same call. The check runs under the task's bundle write
    /// lock, so racing writers with one ID append it once, and the row keeps
    /// the ID for later replays. A comment without an ID, or with a blank one,
    /// always appends. Another task's comment with the same ID never matches.
    pub(crate) fn update_task_history(
        &self,
        id: &str,
        fields: &TaskHistoryUpdateParams,
    ) -> Result<crate::contract::TaskRow, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        if fields.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task actor must not be empty".to_string(),
            ));
        }

        reject_forged_grant(&fields.append_history)?;
        if is_operator_comment_only(fields) {
            self.with_task_comment_lock(id, |claimed| self.write_task_history(id, fields, claimed))
        } else {
            self.with_task_lock(id, || self.write_task_history(id, fields, false))
        }
    }

    /// Publish the body documents. They are separate files, and the post-write
    /// row is the in-memory bundle, so it has to carry the same bytes just
    /// published or a caller would observe the pre-write description,
    /// criteria, plan or execution summary.
    fn write_document_bodies(
        &self,
        id: &str,
        bundle: &mut TaskBundleV2,
        fields: &TaskDocumentUpdateParams,
    ) -> Result<(), OrbitError> {
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
        Ok(())
    }

    /// One history write under the lock its caller took. `claimed` is true
    /// only for an operator comment admitted under an execution claim.
    fn write_task_history(
        &self,
        id: &str,
        fields: &TaskHistoryUpdateParams,
        claimed: bool,
    ) -> Result<crate::contract::TaskRow, OrbitError> {
        let mut bundle = self.read_existing_bundle(id)?;
        let mut pending = PendingWriteGuard::begin(&self.bundle_store.bundle_path(id)?)?;
        let mut pending_events: Vec<TaskEventRowV2> = Vec::new();
        let mut pending_comments: Vec<TaskCommentRowV2> = Vec::new();
        let now = Utc::now();
        let current_status = bundle.envelope.status;
        if let Err(error) =
            guard_expected_status(id, fields.expected_status.as_deref(), current_status)
        {
            pending.finish();
            return Err(error);
        }
        let target_status = fields.status.unwrap_or(current_status);
        self.guard_work_start(&bundle, target_status, fields.status_event.as_deref())?;
        let status_transition =
            (target_status != current_status).then_some((current_status, target_status));
        let creation_sealed_at =
            record_agent_attribution(&mut bundle, fields, claimed, now, &mut pending_events)?;
        // Retry work commits ahead of the transition into done, so no
        // crash or later read failure can leave done without it.
        let pending_resolves =
            pending_done_resolves(&bundle, &fields.actor, now, status_transition);
        append_history_entries(
            &mut bundle,
            fields.append_history.iter().chain(&pending_resolves),
            &mut pending_events,
        );
        append_comment_rows(&mut bundle, &fields.append_comments, &mut pending_comments);
        append_status_event(
            &mut bundle,
            fields,
            status_transition,
            now,
            &mut pending_events,
        );

        let publishes_envelope = !fields.append_history.is_empty()
            || !pending_comments.is_empty()
            || fields.status.is_some()
            || fields.status_event.is_some()
            || fields.status_note.is_some();
        // A claimed task's creation grant is the claim's to keep: a comment
        // admitted under it records no new grant.
        if publishes_envelope && !claimed {
            let sealed_at = match creation_sealed_at {
                Some(sealed_at) => sealed_at,
                None => {
                    let context_files = bundle.envelope.context_files.clone();
                    append_creation_grant(
                        &mut bundle,
                        &context_files,
                        &[],
                        &fields.actor,
                        now,
                        &mut pending_events,
                    )?
                }
            };
            bundle.envelope.context_creation_sealed_at = sealed_at;
        }
        stamp_pending_events(
            &mut bundle.events,
            &mut pending_events,
            fields.write_provenance.as_ref(),
        );
        self.bundle_store.append_events(id, &pending_events)?;
        self.bundle_store.append_comments(id, &pending_comments)?;
        if publishes_envelope {
            bundle.envelope.status = target_status;
            bundle.envelope.updated_at = now;
            self.bundle_store.rewrite_envelope(id, &bundle.envelope)?;
            pending.finish();
            self.publish_task_index_and_lease_notices(&bundle.envelope, "task history update")?;
        } else {
            pending.finish();
        }
        // Same post-write row as a document update: history runs after
        // the document commit, so this bundle includes both.
        self.row_from_bundle(bundle)
    }

    /// Refuse a write that starts ordinary work on a footprint the
    /// coordination boundary rejects.
    fn guard_work_start(
        &self,
        bundle: &TaskBundleV2,
        target_status: TaskStatus,
        status_event: Option<&str>,
    ) -> Result<(), OrbitError> {
        if target_status == TaskStatus::InProgress
            && starts_ordinary_work(status_event)
            && let Some(boundary) = &self.coordination
        {
            boundary.guard_ordinary_footprint(target_status, &bundle.envelope.context_files)?;
        }
        Ok(())
    }
}

/// What a document update compares its result against to decide which
/// history rows it owes.
struct DocumentOriginals {
    envelope: TaskEnvelopeV2,
    description: String,
    acceptance: String,
    plan: String,
    execution_summary: String,
}

impl DocumentOriginals {
    fn capture(bundle: &TaskBundleV2) -> Self {
        Self {
            envelope: bundle.envelope.clone(),
            description: bundle.description.clone(),
            acceptance: bundle.acceptance.clone(),
            plan: bundle.plan.clone(),
            execution_summary: bundle.execution_summary.clone(),
        }
    }
}

/// What [`apply_envelope_fields`] learned while editing the envelope.
struct EnvelopeEdits {
    /// Whether any envelope field was addressed by the update.
    changed: bool,
    /// The title the update replaced, when it changed it.
    previous_title: Option<String>,
    /// The grant seal, when the update carried `context_files`.
    creation_sealed_at: Option<Option<chrono::DateTime<Utc>>>,
}

/// The status events that begin ordinary work on a task, shared by the write
/// guard and its pre-flight form.
fn starts_ordinary_work(status_event: Option<&str>) -> bool {
    matches!(
        status_event,
        Some("started" | "pulled_by" | "resume_readmitted")
    )
}

/// [ORB-16723] An operator's comment is the one unscoped write an
/// active execution claim admits: its only effect is appending one
/// comment to the append-only log, where the claim's steering reads it.
/// Any other effect keeps the ordinary fence and its refusal.
fn is_operator_comment_only(fields: &TaskHistoryUpdateParams) -> bool {
    fields.operator_comment
        && fields.append_comments.len() == 1
        && fields.status.is_none()
        && fields.status_event.is_none()
        && fields.status_note.is_none()
        && fields.status_decision.is_none()
        && fields.append_history.is_empty()
        && fields.expected_status.is_none()
        && fields.planned_by.is_none()
        && fields.implemented_by.is_none()
}

/// [ORB-11305] Compare-and-set, evaluated against the bundle just re-read
/// under the exclusive lock. A caller that decided this write was allowed
/// from an earlier read loses here rather than silently overwriting whatever
/// landed in between.
fn guard_expected_status(
    id: &str,
    expected: Option<&[TaskStatus]>,
    current: TaskStatus,
) -> Result<(), OrbitError> {
    match expected {
        Some(expected) if !expected.contains(&current) => Err(OrbitError::InvalidInput(format!(
            "task '{id}' status changed to '{current}' before this write; \
             expected one of [{}]",
            expected
                .iter()
                .map(TaskStatus::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ))),
        _ => Ok(()),
    }
}

/// Apply `planned_by` / `implemented_by` and record the edit, returning the
/// grant seal when this call carried the grant (`None` when it did not).
fn record_agent_attribution(
    bundle: &mut TaskBundleV2,
    fields: &TaskHistoryUpdateParams,
    claimed: bool,
    now: chrono::DateTime<Utc>,
    pending_events: &mut Vec<TaskEventRowV2>,
) -> Result<Option<Option<chrono::DateTime<Utc>>>, OrbitError> {
    if fields.planned_by.is_none() && fields.implemented_by.is_none() {
        return Ok(None);
    }
    let previous = bundle.envelope.clone();
    if let Some(value) = &fields.planned_by {
        bundle.envelope.planned_by = value.clone();
    }
    if let Some(value) = &fields.implemented_by {
        bundle.envelope.implemented_by = value.clone();
    }
    if bundle.envelope != previous {
        let event = event_row(
            next_event_id(&bundle.events),
            now,
            &fields.actor,
            TASK_FIELDS_UPDATED_EVENT,
            Some(task_fields_updated_note(&[])),
        );
        record_event(bundle, pending_events, event);
    }
    // The document write this replaces sealed any due grant here,
    // ahead of the status event; keep that history order.
    if claimed {
        return Ok(None);
    }
    let context_files = bundle.envelope.context_files.clone();
    Ok(Some(append_creation_grant(
        bundle,
        &context_files,
        &[],
        &fields.actor,
        now,
        pending_events,
    )?))
}

/// The retry-resolution entries owed by a transition into done.
fn pending_done_resolves(
    bundle: &TaskBundleV2,
    actor: &str,
    now: chrono::DateTime<Utc>,
    status_transition: Option<(TaskStatus, TaskStatus)>,
) -> Vec<TaskHistoryEntry> {
    if !status_transition.is_some_and(|(_, to)| to == TaskStatus::Done) {
        return Vec::new();
    }
    pending_resolves_entries(
        &named_friction_targets(
            &bundle.envelope.relations,
            [bundle.description.as_str(), bundle.acceptance.as_str()],
        ),
        actor,
        now,
    )
}

/// Append caller-supplied history entries verbatim, in order.
fn append_history_entries<'a>(
    bundle: &mut TaskBundleV2,
    entries: impl Iterator<Item = &'a TaskHistoryEntry>,
    pending_events: &mut Vec<TaskEventRowV2>,
) {
    for entry in entries {
        let event = TaskEventRowV2 {
            from_status: entry.from_status,
            to_status: entry.to_status,
            decision: entry.decision.clone(),
            surface: entry.surface,
            caller: entry.caller.clone(),
            ..event_row(
                next_event_id(&bundle.events),
                entry.at,
                &entry.by,
                entry.event.clone(),
                entry.note.clone(),
            )
        };
        record_event(bundle, pending_events, event);
    }
}

/// Append each comment except a replay of a nonblank operation id this task's
/// comment log already holds; see [`TaskV2Store::update_task_history`].
fn append_comment_rows(
    bundle: &mut TaskBundleV2,
    comments: &[TaskComment],
    pending_comments: &mut Vec<TaskCommentRowV2>,
) {
    for comment in comments {
        if comment.operation_id.as_ref().is_some_and(|operation_id| {
            !operation_id.trim().is_empty()
                && bundle
                    .comments
                    .iter()
                    .any(|row| row.operation_id.as_ref() == Some(operation_id))
        }) {
            continue;
        }
        let row = TaskCommentRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            comment_id: format!("C-{:04}", next_sequence(&bundle.comments, "C-")),
            at: comment.at,
            by: comment.by.clone(),
            body: comment.message.clone(),
            operation_id: comment.operation_id.clone(),
        };
        pending_comments.push(row.clone());
        bundle.comments.push(row);
    }
}

/// The status event for an explicit `status_event` or a status transition.
fn append_status_event(
    bundle: &mut TaskBundleV2,
    fields: &TaskHistoryUpdateParams,
    status_transition: Option<(TaskStatus, TaskStatus)>,
    now: chrono::DateTime<Utc>,
    pending_events: &mut Vec<TaskEventRowV2>,
) {
    let Some(event_type) = fields
        .status_event
        .clone()
        .or_else(|| status_transition.map(|_| "status_changed".to_string()))
    else {
        return;
    };
    let event = TaskEventRowV2 {
        from_status: status_transition.map(|(from, _)| from),
        to_status: status_transition.map(|(_, to)| to),
        decision: fields.status_decision.clone(),
        ..event_row(
            next_event_id(&bundle.events),
            now,
            &fields.actor,
            event_type,
            fields.status_note.clone(),
        )
    };
    record_event(bundle, pending_events, event);
}

/// A history row with no status transition, receipt, decision or write
/// surface; callers set the few fields their event carries.
fn event_row(
    event_id: String,
    at: chrono::DateTime<Utc>,
    by: &str,
    event_type: impl Into<String>,
    note: Option<String>,
) -> TaskEventRowV2 {
    TaskEventRowV2 {
        schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
        event_id,
        at,
        by: by.to_string(),
        event_type: event_type.into(),
        note,
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
        surface: None,
        caller: None,
    }
}

/// Queue `event` for the pending append and keep the in-memory bundle in step.
fn record_event(
    bundle: &mut TaskBundleV2,
    pending_events: &mut Vec<TaskEventRowV2>,
    event: TaskEventRowV2,
) {
    pending_events.push(event.clone());
    bundle.events.push(event);
}

/// Apply every envelope field the update addresses, in the order their
/// refusals are reported.
fn apply_envelope_fields(
    bundle: &mut TaskBundleV2,
    fields: &TaskDocumentUpdateParams,
    updated_at: chrono::DateTime<Utc>,
    pending_events: &mut Vec<TaskEventRowV2>,
) -> Result<EnvelopeEdits, OrbitError> {
    let mut edits = EnvelopeEdits {
        changed: false,
        previous_title: None,
        creation_sealed_at: None,
    };
    if let Some(value) = &fields.title {
        if value.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task title must not be empty".to_string(),
            ));
        }
        if *value != bundle.envelope.title {
            edits.previous_title = Some(bundle.envelope.title.clone());
        }
        bundle.envelope.title = value.clone();
        edits.changed = true;
    }
    if let Some(value) = &fields.tags {
        let tags = normalize_task_tags(value.clone());
        validate_os_tags(&tags)?;
        bundle.envelope.tags = tags;
        edits.changed = true;
    }
    if let Some(value) = &fields.granted_tools {
        bundle.envelope.granted_tools = orbit_types::task::normalize_required_tools(value.clone());
        edits.changed = true;
    }
    if let Some(value) = &fields.requires_labels {
        bundle.envelope.requires_labels = orbit_types::task::normalize_host_labels(
            value.clone(),
            orbit_types::task::REQUIRES_LABELS_FIELD,
        )?;
        edits.changed = true;
    }
    if let Some(value) = fields.not_before {
        bundle.envelope.not_before = value;
        edits.changed = true;
    }
    if let Some(value) = &fields.context_files {
        edits.creation_sealed_at = Some(append_creation_grant(
            bundle,
            value,
            &fields.context_creation,
            &fields.actor,
            updated_at,
            pending_events,
        )?);
        bundle.envelope.context_files = value.clone();
        edits.changed = true;
    } else if !fields.context_creation.is_empty() {
        return Err(OrbitError::InvalidInput(
            "creation authorization requires the context_files it applies to".to_string(),
        ));
    }
    edits.changed |= apply_attribution_fields(&mut bundle.envelope, fields);
    edits.changed |= apply_relation_fields(&mut bundle.envelope, fields);
    edits.changed |= apply_job_run_binding(&mut bundle.envelope, fields)?;
    edits.changed |= apply_assignment_fields(&mut bundle.envelope, fields);
    Ok(edits)
}

/// Authorship, ranking and PR fields; returns whether any was addressed.
fn apply_attribution_fields(
    envelope: &mut TaskEnvelopeV2,
    fields: &TaskDocumentUpdateParams,
) -> bool {
    let mut changed = false;
    if let Some(value) = &fields.created_by {
        envelope.created_by = value.clone();
        changed = true;
    }
    if let Some(value) = &fields.planned_by {
        envelope.planned_by = value.clone();
        changed = true;
    }
    if let Some(value) = &fields.implemented_by {
        envelope.implemented_by = value.clone();
        changed = true;
    }
    if let Some(value) = fields.priority {
        envelope.priority = value;
        changed = true;
    }
    if let Some(value) = fields.complexity {
        envelope.complexity = Some(value);
        changed = true;
    }
    if let Some(value) = fields.task_type {
        envelope.task_type = value;
        changed = true;
    }
    if let Some(value) = &fields.pr_status {
        envelope.pr_status = value.clone();
        changed = true;
    }
    changed
}

/// Dependency, regression-source and whole-list relation rewrites; returns
/// whether any was addressed.
fn apply_relation_fields(envelope: &mut TaskEnvelopeV2, fields: &TaskDocumentUpdateParams) -> bool {
    let mut changed = false;
    if let Some(value) = &fields.dependencies {
        replace_relations(
            &mut envelope.relations,
            TaskRelationType::BlockedBy,
            value.iter().cloned().map(|target| TaskRelation {
                relation_type: TaskRelationType::BlockedBy,
                target,
                regression: None,
            }),
        );
        changed = true;
    }
    if let Some(value) = &fields.source_task_id {
        // Re-setting the same culprit keeps the link's classification,
        // including its recorded sites.
        let replacements = value
            .iter()
            .cloned()
            .map(|target| {
                let regression = envelope
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
            &mut envelope.relations,
            TaskRelationType::RegressionFrom,
            replacements,
        );
        changed = true;
    }
    if let Some(value) = &fields.relations {
        envelope.relations = value.clone();
        changed = true;
    }
    changed
}

/// The run binding and its immutable execution location; returns whether the
/// update addressed it.
fn apply_job_run_binding(
    envelope: &mut TaskEnvelopeV2,
    fields: &TaskDocumentUpdateParams,
) -> Result<bool, OrbitError> {
    let Some(value) = &fields.job_run_id else {
        return Ok(false);
    };
    if value != &envelope.job_run_id {
        envelope.job_run_machine = fields.job_run_machine.clone().flatten();
    } else if let Some(location) = &fields.job_run_machine
        && location != &envelope.job_run_machine
    {
        return Err(OrbitError::InvalidInput(
            "execution location is immutable for a run binding".into(),
        ));
    }
    if value.is_none() {
        envelope.job_run_machine = None;
    }
    envelope.job_run_id = value.clone();
    Ok(true)
}

/// Crew, orchestrator, origin and external references; returns whether any
/// was addressed.
fn apply_assignment_fields(
    envelope: &mut TaskEnvelopeV2,
    fields: &TaskDocumentUpdateParams,
) -> bool {
    let mut changed = false;
    if let Some(value) = &fields.crew {
        envelope.crew = value.clone();
        // A crew replacement without provenance is an explicit pin.
        envelope.crew_source = value.as_ref().map(|_| CrewSource::Explicit);
        changed = true;
    }
    if let Some(value) = &fields.crew_source {
        envelope.crew_source = value.clone();
        changed = true;
    }
    if let Some(value) = &fields.orchestrator {
        envelope.orchestrator = value.clone();
        changed = true;
    }
    if let Some(value) = fields.origin {
        envelope.origin = Some(value);
        changed = true;
    }
    if let Some(value) = &fields.external_refs {
        envelope.external_refs = value.clone();
        changed = true;
    }
    changed
}

/// Append the `renamed`, `tags_changed` and `task_fields_updated` rows the
/// update owes, in that order.
fn record_document_events(
    bundle: &mut TaskBundleV2,
    fields: &TaskDocumentUpdateParams,
    originals: &DocumentOriginals,
    previous_title: Option<String>,
    pending_events: &mut Vec<TaskEventRowV2>,
) {
    if let Some(previous) = previous_title {
        // ORB-10311: capture both titles so the rename is auditable from
        // history alone rather than an empty `renamed` marker.
        let note = format!("\"{previous}\" → \"{}\"", bundle.envelope.title);
        let event = event_row(
            next_event_id(&bundle.events),
            Utc::now(),
            &fields.actor,
            "renamed",
            Some(note),
        );
        record_event(bundle, pending_events, event);
    }

    if bundle.envelope.tags != originals.envelope.tags {
        let event = tags_changed_event(
            bundle,
            &fields.actor,
            Utc::now(),
            originals.envelope.tags.clone(),
            bundle.envelope.tags.clone(),
        );
        record_event(bundle, pending_events, event);
    }

    // The note names each changed document, so an edit to what the
    // task asks reads apart from run bookkeeping.
    let changed_documents = changed_document_names(fields, originals);
    if unrecorded_envelope_changed(bundle, fields, originals)
        || !changed_documents.is_empty()
        || !fields.context_creation.is_empty()
    {
        let event = event_row(
            next_event_id(&bundle.events),
            Utc::now(),
            &fields.actor,
            TASK_FIELDS_UPDATED_EVENT,
            Some(task_fields_updated_note(&changed_documents)),
        );
        record_event(bundle, pending_events, event);
    }
}

/// Whether the envelope changed in a way no earlier row of this update
/// already records.
///
/// Drain approval snapshots use task history to distinguish a real operator
/// edit from the creation-grant row that merely re-seals unchanged intent.
/// Record content and scope changes alongside the document write so the grant
/// row cannot hide them.
fn unrecorded_envelope_changed(
    bundle: &TaskBundleV2,
    fields: &TaskDocumentUpdateParams,
    originals: &DocumentOriginals,
) -> bool {
    let mut prior = originals.envelope.clone();
    prior.title = bundle.envelope.title.clone();
    prior.crew = bundle.envelope.crew.clone();
    prior.crew_source = bundle.envelope.crew_source.clone();
    if fields.source_task_id.is_some() && fields.relations.is_none() {
        prior
            .relations
            .retain(|relation| relation.relation_type != TaskRelationType::RegressionFrom);
        prior.relations.extend(
            bundle
                .envelope
                .relations
                .iter()
                .filter(|relation| relation.relation_type == TaskRelationType::RegressionFrom)
                .cloned(),
        );
    }
    prior != bundle.envelope
}

/// The body documents whose content the update actually changed.
fn changed_document_names(
    fields: &TaskDocumentUpdateParams,
    originals: &DocumentOriginals,
) -> Vec<&'static str> {
    [
        (
            DESCRIPTION_DOCUMENT,
            fields
                .description
                .as_ref()
                .is_some_and(|value| value != &originals.description),
        ),
        (
            ACCEPTANCE_CRITERIA_DOCUMENT,
            fields
                .acceptance_criteria
                .as_ref()
                .is_some_and(|value| render_acceptance(value) != originals.acceptance),
        ),
        (
            PLAN_DOCUMENT,
            fields
                .plan
                .as_ref()
                .is_some_and(|value| value != &originals.plan),
        ),
        (
            EXECUTION_SUMMARY_DOCUMENT,
            fields
                .execution_summary
                .as_ref()
                .is_some_and(|value| value != &originals.execution_summary),
        ),
    ]
    .into_iter()
    .filter_map(|(document, changed)| changed.then_some(document))
    .collect()
}

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
        decision: Some(TaskDecision::TagsChanged(TagsChanged {
            previous: previous.clone(),
            tags: tags.clone(),
        })),
        ..event_row(
            next_event_id(&bundle.events),
            at,
            actor,
            TAGS_CHANGED_EVENT,
            Some(format!(
                "tags changed: {} → {}",
                render(&previous),
                render(&tags)
            )),
        )
    }
}
