use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, DESKTOP_TASK_EDIT, authorize,
};
use orbit_common::{
    OrbitError,
    security::{redaction::redact_all, release::sha256_hex},
};
use orbit_store::contracts::{AtomicTaskMutationOutcome, DesktopTaskMutationParams};
use orbit_types::{
    desktop::*,
    record::OrbitEvent,
    task::{Task, TaskHistoryEntry, TaskStatus},
    tool::ToolSessionContext,
};

use crate::OrbitRuntime;
use crate::application::task::{
    SYSTEM_ACTOR_LABEL, TaskAddParams, TaskUpdateParams, helpers::effective_actor_label,
    resolves::pending_resolves_history,
};

use super::validation::{bounded_text, criteria, invalid};

fn sanitize(fields: &mut DesktopTaskFields) {
    fields.title = fields.title.as_ref().map(|v| redact_all(v));
    fields.description = fields.description.as_ref().map(|v| redact_all(v));
    if let Some(v) = &mut fields.acceptance_criteria {
        for c in v {
            *c = redact_all(c);
        }
    }
}

impl OrbitRuntime {
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
        if matches!(&request.operation, DesktopTaskOperation::Edit { .. }) {
            authorize(
                &DESKTOP_TASK_EDIT,
                &CallerCapabilities::resolve(&CallerEnvelope::mcp_session(session)),
            )
            .map_err(|denial| OrbitError::CapabilityDenied(denial.to_string()))?;
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
                return self.desktop_write_result(&task.id, true, None, session);
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
            return self.desktop_write_result(&task.id, false, None, session);
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
            return self.desktop_write_result(&id, true, Some(&request.request_id), session);
        }
        let pull_request = if let DesktopTaskOperation::Review { verdict, .. } = &request.operation
        {
            self.desktop_observe_pr_head(&self.get_task(&id)?, verdict)?
        } else {
            None
        };
        let head = pull_request.as_ref().map(|pr| pr.head.clone());
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
            let mut crew_source = None;
            let mut comment = None;
            let mut status = None;
            let mut append_history = Vec::new();
            match &request.operation {
                DesktopTaskOperation::Edit { fields: f, .. } => {
                    if matches!(task.status, TaskStatus::Done | TaskStatus::Archived) {
                        return Err(invalid("terminal tasks cannot be edited through desktop"));
                    }
                    if let Some(target) = f.status {
                        self.desktop_status_allowed(&task, target, session)?;
                        if task.status == TaskStatus::Proposed
                            && target == TaskStatus::Backlog
                            && (f.title.is_some()
                                || f.description.is_some()
                                || f.acceptance_criteria.is_some()
                                || f.priority.is_some()
                                || f.crew.is_some())
                        {
                            return Err(invalid(
                                "proposal approval cannot be combined with field edits",
                            ));
                        }
                        status = Some(target);
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
                    // Desktop edits carry no operator-decision grant. An
                    // attributed agent identity is still refused if a later
                    // edit adds a light-delivery tag [ORB-16872].
                    let writer = super::super::update::task_field_writer(
                        false,
                        agent.as_deref(),
                        model.as_deref(),
                        &actor,
                    );
                    let validated = self.validate_and_normalize_task_field_edits(
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
                        writer,
                    )?;
                    crew_source = validated.crew_source;
                    let v = validated.params;
                    fields = DesktopTaskFields {
                        status: None,
                        title: v.title,
                        description: v.description,
                        acceptance_criteria: v.acceptance_criteria,
                        priority: v.priority,
                        crew: v.crew.map(|crew| crew.unwrap_or_default()),
                    };
                }
                DesktopTaskOperation::Comment { comment: c, .. } => {
                    bounded_text(c, true)?;
                    comment = Some(c.clone());
                }
                DesktopTaskOperation::Review {
                    verdict, complete, ..
                } => {
                    self.desktop_validate_verdict(&task, verdict, pull_request.as_ref())?;
                    if *complete {
                        if verdict.decision != DesktopReviewDecision::Accept {
                            return Err(invalid("changes requested cannot complete a task"));
                        }
                        self.desktop_completion_allowed(&task, session)?;
                        if verdict.expected_head != head {
                            return Err(invalid("reviewed PR head changed or cannot be verified"));
                        }
                        if let Some(reason) = pull_request
                            .as_ref()
                            .and_then(|pr| pr.completion_refusal.as_deref())
                        {
                            return Err(invalid(reason));
                        }
                        self.ensure_resolves_are_workspace_local(&task)?;
                        status = Some(TaskStatus::Done);
                        // Committed with the receipt, so a replay retries
                        // targets the first call never reached.
                        append_history = pending_resolves_history(&task);
                    }
                    // The audit names where the reviewed run executed and
                    // which pull request the verdict observed.
                    let mut audit = format!(
                        "desktop_review_verdict={}\nreviewed_revision={}\nrequest_id={}",
                        serde_json::to_string(verdict).map_err(|e| invalid(&e.to_string()))?,
                        expected_revision,
                        request.request_id
                    );
                    if let Some(location) = &task.job_run_machine {
                        audit.push_str(&format!("\nexecution_machine={}", location.machine_id));
                    }
                    if let Some(url) = pull_request.as_ref().and_then(|pr| pr.url.as_deref()) {
                        audit.push_str(&format!("\npull_request={url}"));
                    }
                    comment = Some(audit);
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
                        crew_source,
                        comment,
                        status,
                        append_history,
                    },
                )?;
                let event =
                    if task.status == TaskStatus::Proposed && status == Some(TaskStatus::Backlog) {
                        OrbitEvent::TaskProposalApproved {
                            id: id.clone(),
                            approved_by: actor.clone(),
                        }
                    } else {
                        OrbitEvent::TaskUpdated { id: id.clone() }
                    };
                Ok((result, event))
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
            Some(&request.request_id),
            session,
        )
    }

    /// The post-commit side effects and snapshot of a desktop write. The
    /// first call and every replay of `request_id` run the same steps, each
    /// finishing only what the task's history records as unfinished, so a
    /// replay completes what a failed first call left behind.
    pub(in crate::application::task) fn desktop_write_result(
        &self,
        id: &str,
        replayed: bool,
        request_id: Option<&str>,
        session: &ToolSessionContext,
    ) -> Result<DesktopTaskWriteResult, OrbitError> {
        let refresh = || {
            let task = self.desktop_side_effect_task(id)?;
            self.stores().task_records().index_task(&task);
            if let Some(request_id) = request_id {
                self.desktop_close_transition_prs(request_id, &task);
            }
            // A completion committed its targets as pending with its
            // receipt; any other write only retries earlier failures.
            self.record_resolves_side_effects(task.status, &task);
            Ok(DesktopTaskWriteResult {
                snapshot: self.desktop_task_snapshot(id, session)?,
                replayed,
            })
        };
        refresh().map_err(|error: OrbitError| OrbitError::DesktopWriteAccepted {
            task_id: id.into(),
            reason: error.to_string(),
        })
    }

    fn desktop_side_effect_task(&self, id: &str) -> Result<Task, OrbitError> {
        #[cfg(test)]
        if FAIL_SIDE_EFFECT_READ.with(|fail| fail.replace(false)) {
            return Err(OrbitError::Execution(
                "injected post-lock task read failure".to_string(),
            ));
        }
        self.get_task(id)
    }

    /// Close the PRs of the terminal transition `request_id` committed,
    /// unless a [`DESKTOP_PR_CLOSURE_EVENT`] records that the pass already
    /// ran. A later status decision wins over the request's transition.
    fn desktop_close_transition_prs(&self, request_id: &str, task: &Task) {
        let history = match self.get_task_history(&task.id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id,
                    %error,
                    "reading the desktop receipt for PR closure failed"
                );
                return;
            }
        };
        let receipt = format!("desktop_request={request_id} ");
        let Some((previous, committed)) = history
            .iter()
            .find(|h| {
                h.event == "desktop_mutation"
                    && h.note.as_deref().is_some_and(|n| n.starts_with(&receipt))
            })
            .and_then(|h| Some((h.from_status?, h.to_status?)))
        else {
            return;
        };
        let note = format!("desktop_request={request_id}");
        if !committed.is_terminal()
            || task.status != committed
            || history
                .iter()
                .any(|h| h.event == DESKTOP_PR_CLOSURE_EVENT && h.note.as_deref() == Some(&note))
        {
            return;
        }
        if !self.close_task_prs_after_transition(previous, task, None) {
            return;
        }
        let entry = TaskHistoryEntry {
            at: chrono::Utc::now(),
            by: SYSTEM_ACTOR_LABEL.to_string(),
            event: DESKTOP_PR_CLOSURE_EVENT.to_string(),
            note: Some(note),
            from_status: None,
            to_status: None,
            operation_id: None,
            decision: None,
        };
        if let Err(error) = self.append_system_history(&task.id, vec![entry]) {
            tracing::warn!(
                task_id = %task.id,
                %error,
                "recording the desktop PR closure on the task failed"
            );
        }
    }
}

/// Task history event recording that the PR closure of a desktop request's
/// terminal transition ran. Its note is `desktop_request=<id>`.
const DESKTOP_PR_CLOSURE_EVENT: &str = "desktop_pr_closure";

#[cfg(test)]
thread_local! {
    pub(super) static FAIL_SIDE_EFFECT_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
