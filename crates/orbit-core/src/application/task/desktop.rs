//! Guarded desktop authoring and evidence-bound review.
use super::{
    TaskAddParams, TaskUpdateParams, helpers::effective_actor_label,
    lifecycle::ensure_status_change_allowed,
};
use crate::OrbitRuntime;
use chrono::Utc;
use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, DESKTOP_TASK_COMPLETE, authorize,
};
use orbit_common::{
    OrbitError,
    security::{redaction::redact_all, release::sha256_hex},
};
use orbit_store::contracts::{AtomicTaskMutationOutcome, DesktopTaskMutationParams};
use orbit_types::{
    desktop::*,
    record::OrbitEvent,
    task::{GITHUB_PR_EXTERNAL_REF_SYSTEM, Task, TaskStatus},
    tool::ToolSessionContext,
};

fn invalid(message: &str) -> OrbitError {
    OrbitError::InvalidInput(message.into())
}
fn action(result: Result<(), OrbitError>) -> DesktopAction {
    match result {
        Ok(()) => DesktopAction {
            enabled: true,
            reason: None,
        },
        Err(e) => DesktopAction {
            enabled: false,
            reason: Some(e.to_string()),
        },
    }
}
fn bounded_text(value: &str, required: bool) -> Result<(), OrbitError> {
    if value.len() > 32_768 || (required && value.trim().is_empty()) {
        return Err(invalid(
            "desktop text must be non-empty when required and at most 32768 bytes",
        ));
    }
    Ok(())
}
fn criteria(values: &[String]) -> Result<(), OrbitError> {
    if values.is_empty() || values.len() > 100 {
        return Err(invalid(
            "desktop tasks require 1 to 100 acceptance criteria",
        ));
    }
    for value in values {
        bounded_text(value, true)?;
    }
    Ok(())
}
fn sanitize(fields: &mut DesktopTaskFields) {
    fields.title = fields.title.as_ref().map(|v| redact_all(v));
    fields.description = fields.description.as_ref().map(|v| redact_all(v));
    if let Some(v) = &mut fields.acceptance_criteria {
        for c in v {
            *c = redact_all(c);
        }
    }
}
fn clip(value: &mut String, max: usize, field: &str, truncated: &mut Vec<String>) {
    let safe = redact_all(value);
    if safe != *value {
        truncated.push(field.into());
    }
    *value = safe;
    if value.len() > max {
        let boundary = orbit_common::text::floor_char_boundary(value, max);
        value.truncate(boundary);
        truncated.push(field.into());
    }
}
fn truncate_task(task: &mut Task, truncated: &mut Vec<String>) {
    clip(&mut task.title, 512, "task.title", truncated);
    clip(&mut task.description, 32768, "task.description", truncated);
    clip(&mut task.plan, 16384, "task.plan", truncated);
    clip(
        &mut task.execution_summary,
        16384,
        "task.execution_summary",
        truncated,
    );
    if task.acceptance_criteria.len() > 100 {
        task.acceptance_criteria.truncate(100);
        truncated.push("task.acceptance_criteria".into());
    }
    for (index, criterion) in task.acceptance_criteria.iter_mut().enumerate() {
        clip(
            criterion,
            2048,
            &format!("task.acceptance_criteria[{index}]"),
            truncated,
        );
    }
    for (name, values) in [
        ("task.context_files", &mut task.context_files),
        ("task.tags", &mut task.tags),
        ("task.required_tools", &mut task.required_tools),
    ] {
        if values.len() > 100 {
            values.truncate(100);
            truncated.push(name.into());
        }
        for (index, value) in values.iter_mut().enumerate() {
            clip(value, 1024, &format!("{name}[{index}]"), truncated);
        }
    }
    if task.external_refs.len() > 100 {
        task.external_refs.truncate(100);
        truncated.push("task.external_refs".into());
    }
    for reference in &mut task.external_refs {
        clip(&mut reference.id, 1024, "task.external_refs.id", truncated);
        if let Some(url) = &mut reference.url {
            clip(url, 2048, "task.external_refs.url", truncated);
        }
    }
    if task.relations.len() > 100 {
        task.relations.truncate(100);
        truncated.push("task.relations".into());
    }
}

impl OrbitRuntime {
    pub fn desktop_task_snapshot(
        &self,
        id: &str,
        session: &ToolSessionContext,
    ) -> Result<DesktopTaskSnapshot, OrbitError> {
        self.desktop_task_snapshot_page(id, 0, 0, 0, 50, session)
    }
    pub fn desktop_task_snapshot_page(
        &self,
        id: &str,
        comments_offset: usize,
        history_offset: usize,
        artifacts_offset: usize,
        limit: usize,
        session: &ToolSessionContext,
    ) -> Result<DesktopTaskSnapshot, OrbitError> {
        let orbit_store::contracts::DesktopTaskRead {
            mut task,
            revision,
            comments,
            history,
            artifacts,
            write_disabled_reason,
        } = self.stores().tasks().read_desktop_task(id)?;
        let (review_projection, review_reason) =
            match crate::application::review::task_review_projection(self, &task, &artifacts) {
                Ok(Some(value)) => {
                    let bytes = serde_json::to_string(&value)
                        .map_err(|error| OrbitError::Execution(error.to_string()))?;
                    let safe = redact_all(&bytes);
                    if safe.len() > 32768 {
                        (None, Some("structured review evidence exceeds the desktop 32 KiB limit; use the authoritative review artifact".into()))
                    } else {
                        (
                            Some(
                                serde_json::from_str(&safe)
                                    .map_err(|error| OrbitError::Execution(error.to_string()))?,
                            ),
                            None,
                        )
                    }
                }
                Ok(None) => (
                    None,
                    Some("structured review gate evidence is unavailable".into()),
                ),
                Err(error) => (None, Some(error.to_string())),
            };
        let comments_total = comments.len();
        let history_total = history.len();
        let artifacts_total = artifacts.len();
        let limit = limit.clamp(1, 100);
        let mut truncated_fields = Vec::new();
        truncate_task(&mut task, &mut truncated_fields);
        let content_truncated = !truncated_fields.is_empty();
        let comments = comments
            .into_iter()
            .skip(comments_offset)
            .take(limit)
            .enumerate()
            .map(|(index, mut comment)| {
                clip(
                    &mut comment.message,
                    4096,
                    &format!("comments[{index}].message"),
                    &mut truncated_fields,
                );
                clip(
                    &mut comment.by,
                    512,
                    &format!("comments[{index}].by"),
                    &mut truncated_fields,
                );
                comment
            })
            .collect();
        let history = history
            .into_iter()
            .skip(history_offset)
            .take(limit)
            .enumerate()
            .map(|(index, mut history)| {
                if let Some(note) = &mut history.note {
                    clip(
                        note,
                        4096,
                        &format!("history[{index}].note"),
                        &mut truncated_fields,
                    );
                }
                clip(
                    &mut history.by,
                    512,
                    &format!("history[{index}].by"),
                    &mut truncated_fields,
                );
                history
            })
            .collect();
        let artifacts = artifacts
            .into_iter()
            .skip(artifacts_offset)
            .take(limit)
            .map(|artifact| DesktopArtifactMetadata {
                path: artifact.path,
                media_type: artifact.media_type,
                size_bytes: artifact.size_bytes,
                sha256: artifact.sha256,
                created_by: artifact.created_by,
                created_at: artifact.created_at.to_rfc3339(),
            })
            .collect();
        let writable = self.ensure_coordination_task_write_permitted();
        let reason = writable
            .err()
            .map(|e| e.to_string())
            .or(write_disabled_reason);
        let editable =
            !content_truncated && !matches!(task.status, TaskStatus::Done | TaskStatus::Archived);
        let review = !content_truncated && task.status == TaskStatus::Review;
        let completion = if let Some(reason) = &reason {
            Err(invalid(reason))
        } else if content_truncated {
            Err(invalid(
                "task content is truncated; use the full authoritative task before editing or reviewing",
            ))
        } else {
            self.desktop_completion_allowed(&task, session)
        };
        let mut snapshot = DesktopTaskSnapshot {
            schema_version: 1,
            observed_at: Utc::now().to_rfc3339(),
            revision,
            task,
            actions: DesktopTaskActions {
                edit: DesktopAction {
                    enabled: editable && reason.is_none(),
                    reason: reason
                        .clone()
                        .or_else(|| {
                            content_truncated.then(|| {
                                "task content is truncated; use the full authoritative task".into()
                            })
                        })
                        .or_else(|| {
                            (!editable)
                                .then(|| "terminal tasks cannot be edited through desktop".into())
                        }),
                },
                comment: DesktopAction {
                    enabled: reason.is_none(),
                    reason: reason.clone(),
                },
                review: DesktopAction {
                    enabled: review && reason.is_none(),
                    reason: reason
                        .or_else(|| {
                            content_truncated.then(|| {
                                "task content is truncated; use the full authoritative task".into()
                            })
                        })
                        .or_else(|| (!review).then(|| "task must be in review".into())),
                },
                complete: action(completion),
            },
            comments,
            history,
            artifacts,
            comments_total,
            history_total,
            artifacts_total,
            reviewed_head: None,
            reviewed_head_reason: None,
            truncated_fields,
            content_truncated,
            review: review_projection,
            review_reason,
        };
        if snapshot.task.status == TaskStatus::Review
            && snapshot
                .task
                .external_refs
                .iter()
                .any(|r| r.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
        {
            match self.desktop_current_pr_head(&snapshot.task) {
                Ok(head) => snapshot.reviewed_head = head,
                Err(error) => {
                    let reason = error.to_string();
                    snapshot.reviewed_head_reason = Some(reason.clone());
                    snapshot.actions.complete = DesktopAction {
                        enabled: false,
                        reason: Some(reason),
                    };
                }
            }
        }
        if self.stores().tasks().desktop_task_revision(id)? != snapshot.revision {
            return Err(OrbitError::TaskRevisionConflict { task_id: id.into() });
        }
        Ok(snapshot)
    }
    fn desktop_completion_allowed(
        &self,
        task: &Task,
        session: &ToolSessionContext,
    ) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        authorize(
            &DESKTOP_TASK_COMPLETE,
            &CallerCapabilities::resolve(&CallerEnvelope::mcp_session(session)),
        )
        .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))?;
        if task.status != TaskStatus::Review {
            return Err(invalid("desktop completion requires review state"));
        }
        criteria(&task.acceptance_criteria)?;
        let mut pending = task.job_run_id.clone().into_iter().collect::<Vec<_>>();
        let mut visited = std::collections::HashSet::new();
        while let Some(run_id) = pending.pop() {
            if !visited.insert(run_id.clone()) {
                continue;
            }
            let run = self.get_job_run_backend(&run_id)?.ok_or_else(|| {
                invalid(
                    "linked review run is unavailable; completion cannot verify stopped execution",
                )
            })?;
            if matches!(
                run.state,
                orbit_types::workflow::JobRunState::Pending
                    | orbit_types::workflow::JobRunState::Running
                    | orbit_types::workflow::JobRunState::Retrying
            ) {
                return Err(invalid(
                    "linked review run is not stopped; pending, running and retrying execution cannot be completed",
                ));
            }
            if let Some(state) = self.read_run_state(&run_id)? {
                pending.extend(
                    state
                        .child_dispatches
                        .into_iter()
                        .map(|child| child.child_run_id),
                );
            }
        }
        self.ensure_resolves_are_workspace_local(task)?;
        ensure_status_change_allowed(self, task, &TaskUpdateParams::default(), TaskStatus::Done)
    }
    pub fn desktop_task_write(
        &self,
        mut request: DesktopTaskRequest,
        agent: Option<String>,
        model: Option<String>,
        session: &ToolSessionContext,
    ) -> Result<DesktopTaskWriteResult, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if request.request_id.is_empty()
            || request.request_id.len() > 128
            || !request
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(invalid(
                "request_id must contain 1 to 128 ASCII letters, digits, dots, underscores or hyphens",
            ));
        }
        // Redact before digesting and durably writing; retries bind to the safe persisted payload.
        match &mut request.operation {
            DesktopTaskOperation::Create {
                title,
                description,
                acceptance_criteria,
                ..
            } => {
                *title = redact_all(title);
                *description = redact_all(description);
                for c in acceptance_criteria {
                    *c = redact_all(c);
                }
            }
            DesktopTaskOperation::Edit { fields, .. } => sanitize(fields),
            DesktopTaskOperation::Comment { comment, .. } => *comment = redact_all(comment),
            DesktopTaskOperation::Review { verdict, .. } => {
                verdict.rationale = redact_all(&verdict.rationale);
                for e in &mut verdict.evidence {
                    *e = redact_all(e);
                }
                for c in &mut verdict.criteria {
                    c.criterion = redact_all(&c.criterion);
                    for e in &mut c.evidence {
                        *e = redact_all(e);
                    }
                }
            }
        }
        let payload = serde_json::to_vec(&request).map_err(|e| invalid(&e.to_string()))?;
        if payload.len() > 262_144 {
            return Err(invalid("desktop request exceeds 256 KiB"));
        }
        let digest = sha256_hex(&payload);
        let (agent, model) =
            self.try_canonical_agent_model_identity(agent.as_deref(), model.as_deref())?;
        let actor = effective_actor_label(&self.actor().label, agent.as_deref(), model.as_deref())?;
        if let DesktopTaskOperation::Create {
            title,
            description,
            acceptance_criteria,
            priority,
            crew,
        } = request.operation
        {
            bounded_text(&title, true)?;
            bounded_text(&description, false)?;
            criteria(&acceptance_criteria)?;
            let key = format!("desktop-create:{}", request.request_id);
            if let Some(task) = self
                .stores()
                .tasks()
                .lookup_desktop_creation(&key, &digest)?
            {
                return self.desktop_write_result(&task.id, true, session);
            }
            let task = self.add_task_admitted_guarded(
                TaskAddParams {
                    title,
                    description,
                    acceptance_criteria,
                    priority,
                    crew,
                    status: Some(TaskStatus::Proposed),
                    ..Default::default()
                },
                agent,
                model,
                Some(&key),
                Some(&digest),
            )?;
            return self.desktop_write_result(&task.id, false, session);
        }
        let (id, expected_revision) = match &request.operation {
            DesktopTaskOperation::Edit {
                id,
                expected_revision,
                ..
            }
            | DesktopTaskOperation::Comment {
                id,
                expected_revision,
                ..
            }
            | DesktopTaskOperation::Review {
                id,
                expected_revision,
                ..
            } => (id.clone(), expected_revision.clone()),
            DesktopTaskOperation::Create { .. } => unreachable!(),
        };
        // Slow PR observation precedes the bundle lock; the locked revision guards the linkage.
        let receipt_prefix = format!("desktop_request={} ", request.request_id);
        let expected_receipt = format!("{receipt_prefix}digest={digest}");
        if let Some(history) = self.get_task_history(&id)?.iter().find(|h| {
            h.event == "desktop_mutation"
                && h.note
                    .as_deref()
                    .is_some_and(|n| n.starts_with(&receipt_prefix))
        }) {
            if history.note.as_deref() != Some(expected_receipt.as_str()) {
                return Err(invalid("request identity reused with a different payload"));
            }
            return self.desktop_write_result(&id, true, session);
        }
        let head = if let DesktopTaskOperation::Review { verdict, .. } = &request.operation {
            self.desktop_observe_pr_head(&self.get_task(&id)?, verdict)?
        } else {
            None
        };
        let mut outcome = None;
        self.stores().tasks().with_task_write_lock(&id, &mut || {
            let key = format!("desktop_request={} ", request.request_id);
            let receipt = format!("{key}digest={digest}");
            if let Some(h) = self.get_task_history(&id)?.iter().find(|h| {
                h.event == "desktop_mutation"
                    && h.note.as_deref().is_some_and(|n| n.starts_with(&key))
            }) {
                if h.note.as_deref() != Some(receipt.as_str()) {
                    return Err(invalid("request identity reused with a different payload"));
                }
                outcome = Some(AtomicTaskMutationOutcome::AlreadyApplied);
                return Ok(());
            }
            if self.stores().tasks().desktop_task_revision(&id)? != expected_revision {
                return Err(OrbitError::TaskRevisionConflict {
                    task_id: id.clone(),
                });
            }
            let task = self.get_task(&id)?;
            let mut fields = DesktopTaskFields::default();
            let mut comment = None;
            let mut status = None;
            match &request.operation {
                DesktopTaskOperation::Edit { fields: f, .. } => {
                    if matches!(task.status, TaskStatus::Done | TaskStatus::Archived) {
                        return Err(invalid("terminal tasks cannot be edited through desktop"));
                    }
                    if let Some(v) = &f.title {
                        bounded_text(v, true)?;
                    }
                    if let Some(v) = &f.description {
                        bounded_text(v, false)?;
                    }
                    if let Some(v) = &f.acceptance_criteria {
                        criteria(v)?;
                    }
                    let v = self
                        .validate_and_normalize_task_field_edits(
                            &id,
                            &task,
                            TaskUpdateParams {
                                title: f.title.clone(),
                                description: f.description.clone(),
                                acceptance_criteria: f.acceptance_criteria.clone(),
                                priority: f.priority,
                                crew: f.crew.clone().map(Some),
                                ..Default::default()
                            },
                        )?
                        .params;
                    fields = DesktopTaskFields {
                        title: v.title,
                        description: v.description,
                        acceptance_criteria: v.acceptance_criteria,
                        priority: v.priority,
                        crew: v.crew.flatten(),
                    };
                }
                DesktopTaskOperation::Comment { comment: c, .. } => {
                    bounded_text(c, true)?;
                    comment = Some(c.clone());
                }
                DesktopTaskOperation::Review {
                    verdict, complete, ..
                } => {
                    self.desktop_validate_verdict(&task, verdict)?;
                    if *complete {
                        if verdict.decision != DesktopReviewDecision::Accept {
                            return Err(invalid("changes requested cannot complete a task"));
                        }
                        self.desktop_completion_allowed(&task, session)?;
                        if verdict.expected_head != head {
                            return Err(invalid("reviewed PR head changed or cannot be verified"));
                        }
                        self.ensure_resolves_are_workspace_local(&task)?;
                        status = Some(TaskStatus::Done);
                    }
                    comment = Some(format!(
                        "desktop_review_verdict={}\nreviewed_revision={}\nrequest_id={}",
                        serde_json::to_string(verdict).map_err(|e| invalid(&e.to_string()))?,
                        expected_revision,
                        request.request_id
                    ));
                }
                DesktopTaskOperation::Create { .. } => unreachable!(),
            }
            outcome = Some(self.with_mutation(|| {
                let result = self.stores().tasks().apply_desktop_task_mutation(
                    &id,
                    &DesktopTaskMutationParams {
                        actor: actor.clone(),
                        request_id: request.request_id.clone(),
                        payload_digest: digest.clone(),
                        expected_revision: expected_revision.clone(),
                        fields,
                        comment,
                        status,
                    },
                )?;
                Ok((result, OrbitEvent::TaskUpdated { id: id.clone() }))
            })?);
            Ok(())
        })?;
        if outcome == Some(AtomicTaskMutationOutcome::Stale) {
            return Err(OrbitError::TaskRevisionConflict {
                task_id: id.clone(),
            });
        }
        self.desktop_write_result(
            &id,
            outcome == Some(AtomicTaskMutationOutcome::AlreadyApplied),
            session,
        )
    }
    fn desktop_write_result(
        &self,
        id: &str,
        replayed: bool,
        session: &ToolSessionContext,
    ) -> Result<DesktopTaskWriteResult, OrbitError> {
        let refresh = || {
            let task = self.get_task(id)?;
            self.stores().task_records().index_task(&task);
            if task.status == TaskStatus::Done {
                self.record_resolves_side_effects(&task)?;
            }
            Ok(DesktopTaskWriteResult {
                snapshot: self.desktop_task_snapshot(id, session)?,
                replayed,
            })
        };
        refresh().map_err(|error: OrbitError| OrbitError::Execution(format!("desktop write was accepted; refresh failed: {error}; reconcile by retrying the same request identity before submitting anything new")))
    }

    fn desktop_validate_verdict(
        &self,
        task: &Task,
        verdict: &DesktopReviewVerdict,
    ) -> Result<(), OrbitError> {
        if task.status != TaskStatus::Review {
            return Err(invalid("review decisions require review state"));
        }
        bounded_text(&verdict.rationale, true)?;
        if task.job_run_id != verdict.expected_run_id {
            return Err(invalid("reviewed run binding changed"));
        }
        criteria(&task.acceptance_criteria)?;
        if verdict.criteria.len() != task.acceptance_criteria.len()
            || verdict.evidence.is_empty()
            || verdict.evidence.len() > 100
        {
            return Err(invalid(
                "review verdict must cover every criterion and cite current evidence",
            ));
        }
        let manifest = self.get_task_artifact_manifest(&task.id)?;
        let known = |e: &str| {
            e == "execution_summary" && !task.execution_summary.trim().is_empty()
                || manifest.iter().any(|a| a.path == e)
                || task
                    .external_refs
                    .iter()
                    .any(|r| r.url.as_deref() == Some(e))
                || task.job_run_id.as_deref() == Some(e)
        };
        for e in &verdict.evidence {
            if !known(e) {
                return Err(invalid(
                    "review evidence must reference current summary, run, artifact or external reference",
                ));
            }
        }
        for (expected, actual) in task.acceptance_criteria.iter().zip(&verdict.criteria) {
            if &actual.criterion != expected
                || actual.evidence.is_empty()
                || actual.evidence.len() > 100
                || actual.evidence.iter().any(|e| !known(e))
                || (verdict.decision == DesktopReviewDecision::Accept && !actual.met)
            {
                return Err(invalid(
                    "criterion outcome is missing, unmet, or references unavailable evidence",
                ));
            }
        }
        Ok(())
    }
    fn desktop_observe_pr_head(
        &self,
        task: &Task,
        verdict: &DesktopReviewVerdict,
    ) -> Result<Option<String>, OrbitError> {
        let head = self.desktop_current_pr_head(task)?;
        if verdict.expected_head != head {
            return Err(invalid("linked PR head changed since review"));
        }
        Ok(head)
    }
    fn desktop_current_pr_head(&self, task: &Task) -> Result<Option<String>, OrbitError> {
        let refs: Vec<_> = task
            .external_refs
            .iter()
            .filter(|r| r.system == GITHUB_PR_EXTERNAL_REF_SYSTEM)
            .collect();
        if refs.is_empty() {
            return Ok(None);
        }
        if refs.len() != 1 {
            return Err(invalid(
                "desktop completion requires one unambiguous PR reference",
            ));
        }
        let url = refs[0]
            .url
            .as_deref()
            .ok_or_else(|| invalid("linked PR has no verifiable URL"))?;
        let value = self.run_tool(
            "github.pr.list",
            serde_json::json!({"state":"all","limit":100}),
        )?;
        let head = value["pull_requests"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["url"].as_str() == Some(url)))
            .and_then(|r| r["reported_head_sha"].as_str())
            .filter(|h| !h.is_empty())
            .ok_or_else(|| {
                invalid("current linked PR head unavailable; refresh repository evidence")
            })?;
        Ok(Some(head.into()))
    }
}
