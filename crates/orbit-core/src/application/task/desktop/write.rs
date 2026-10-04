use orbit_common::governance::authorization::{
    CallerCapabilities, CallerEnvelope, DESKTOP_TASK_EDIT, authorize,
};
use orbit_common::{
    OrbitError,
    security::{redaction::redact_all, release::sha256_hex},
};
use orbit_store::contracts::{AtomicTaskMutationOutcome, DesktopTaskMutationParams};
use orbit_types::{desktop::*, record::OrbitEvent, task::TaskStatus, tool::ToolSessionContext};

use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams, helpers::effective_actor_label};

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
            session,
        )
    }
    pub(in crate::application::task) fn desktop_write_result(
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
        refresh().map_err(|error: OrbitError| OrbitError::DesktopWriteAccepted {
            task_id: id.into(),
            reason: error.to_string(),
        })
    }
}
