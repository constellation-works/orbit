//! Listing, showing and answering human decision requests.
//!
//! Requests are raised on held work by block triage and pilot admission
//! (see `orbit_types::task::DecisionRequest`) and live in task history, so
//! listing scans task histories — the shape the block readout already has.
//!
//! [`OrbitRuntime::answer_decision_request_as_human`] is the core apply
//! step every answering surface shares: the operator CLI, the dashboard,
//! and later the delegated channel that submits a human's answer to core.
//! It refuses a stale answer (superseded, expired, or an answer outside
//! the request's allowed set) with the design's named refusals and writes
//! nothing; an allowed answer lands through the human funnel
//! (`update_task_as_human`, or `approve_task_screened` / `reject_task` for
//! approvals) and only then records `decision_answered`, which is what
//! makes the request read `answered`.

use orbit_common::OrbitError;
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DecisionGate, DecisionRequest, DecisionRequestState,
    DecisionResolution, PilotAdmissionVerdict, TaskHistoryEntry, TaskStatus,
};
use serde_json::{Value, json};

use super::TaskRecordUpdateParams;
use super::params::TaskUpdateParams;
use crate::OrbitRuntime;
use crate::application::task::TaskRecords;

/// JSON for one request as the decision surfaces serve it: the stored
/// record with its effective (derived) state.
pub(crate) fn decision_request_json(request: &DecisionRequest) -> Value {
    serde_json::to_value(request).unwrap_or_else(|_| {
        json!({
            "id": request.id,
            "task_id": request.task_id,
            "gate": request.gate.as_str(),
            "state": request.state.as_str(),
        })
    })
}

/// The compact `open_decisions[]` entry `orbit.task.show` carries: the
/// request id, its gate, the answer ids a surface may offer, and expiry.
pub(crate) fn open_decision_json(request: &DecisionRequest) -> Value {
    json!({
        "id": request.id,
        "gate": request.gate.as_str(),
        "answers": request
            .answers
            .iter()
            .map(|answer| answer.id.clone())
            .collect::<Vec<_>>(),
        "expires_at": request.expires_at.to_rfc3339(),
    })
}

impl OrbitRuntime {
    /// Decision requests across the workspace, oldest first. Open requests
    /// only unless `include_closed`; `gate` and `task_id` narrow the list.
    pub fn list_decision_requests(
        &self,
        gate: Option<DecisionGate>,
        task_id: Option<&str>,
        include_closed: bool,
    ) -> Result<Vec<DecisionRequest>, OrbitError> {
        let ids: Vec<String> = match task_id {
            Some(task_id) => vec![task_id.to_string()],
            None => self
                .list_task_metadata()?
                .into_iter()
                .map(|task| task.id)
                .collect(),
        };
        let mut requests = Vec::new();
        for id in &ids {
            for request in self.get_task_decision_requests(id)? {
                if gate.is_some_and(|gate| gate != request.gate) {
                    continue;
                }
                if !include_closed && request.state != DecisionRequestState::Open {
                    continue;
                }
                requests.push(request);
            }
        }
        requests.sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
        Ok(requests)
    }

    /// One decision request by id, with its effective state, or `None`
    /// when no task in the workspace carries it.
    pub fn get_decision_request(
        &self,
        request_id: &str,
    ) -> Result<Option<DecisionRequest>, OrbitError> {
        Ok(self
            .list_decision_requests(None, None, true)?
            .into_iter()
            .find(|request| request.id == request_id))
    }

    /// The task's open decision requests in the compact show form: id,
    /// gate, answer ids and expiry per request.
    pub fn open_decisions_json(&self, task_id: &str) -> Result<Value, OrbitError> {
        Ok(Value::Array(
            self.get_task_decision_requests(task_id)?
                .iter()
                .filter(|request| request.state == DecisionRequestState::Open)
                .map(open_decision_json)
                .collect(),
        ))
    }

    /// Apply `answer_id` to the open request `request_id` as the human
    /// `actor_label`, returning the request as it then reads (`answered`).
    ///
    /// `provenance` names the surface the answer arrived through (for
    /// example `cli` or `dashboard`); it is recorded in the resolution
    /// and the answer comment, never in the writer label, which stays the
    /// human's. This is deliberately the one apply step: a delegated
    /// channel later submits through it after its own checks, and no
    /// surface re-implements what an answer means per gate.
    pub fn answer_decision_request_as_human(
        &self,
        request_id: &str,
        answer_id: &str,
        note: Option<String>,
        actor_label: String,
        provenance: Option<String>,
    ) -> Result<DecisionRequest, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let request = self.get_decision_request(request_id)?.ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "request_not_found: no decision request `{request_id}` in this workspace"
            ))
        })?;
        match request.state {
            DecisionRequestState::Open => {}
            DecisionRequestState::Expired => {
                return Err(OrbitError::InvalidInput(format!(
                    "request_expired: decision request `{request_id}` expired at {}; nothing was applied",
                    request.expires_at.to_rfc3339()
                )));
            }
            DecisionRequestState::Answered => {
                let by = request
                    .resolution
                    .as_ref()
                    .map_or("another answer", |resolution| {
                        resolution.answered_by.as_str()
                    });
                return Err(OrbitError::InvalidInput(format!(
                    "request_superseded: decision request `{request_id}` was already answered by {by}; nothing was applied"
                )));
            }
            DecisionRequestState::Superseded => {
                return Err(OrbitError::InvalidInput(format!(
                    "request_superseded: decision request `{request_id}` no longer matches the hold on task {}; nothing was applied",
                    request.task_id
                )));
            }
        }
        if !request.allows(answer_id) {
            let allowed = request
                .answers
                .iter()
                .map(|answer| answer.id.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(OrbitError::InvalidInput(format!(
                "answer_not_allowed: `{answer_id}` is not an allowed answer for decision request `{request_id}` (allowed: {allowed}); nothing was applied"
            )));
        }
        let note = note
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        self.apply_decision_answer(
            &request,
            answer_id,
            note.as_deref(),
            &actor_label,
            provenance.as_deref(),
        )?;

        let now = chrono::Utc::now();
        let resolution = DecisionResolution {
            request_id: request.id.clone(),
            answer_id: answer_id.to_string(),
            answered_by: actor_label.clone(),
            provenance: provenance.clone(),
            note: note.clone(),
            answered_at: now,
        };
        let answered = TaskHistoryEntry {
            at: now,
            by: actor_label.clone(),
            event: DECISION_ANSWERED_EVENT.to_string(),
            note: serde_json::to_string(&resolution).ok(),
            from_status: None,
            to_status: None,
            operation_id: Some(request.id.clone()),
            decision: None,
        };
        self.stores().task_records().update(
            &request.task_id,
            TaskRecordUpdateParams {
                actor: actor_label,
                append_history: vec![answered],
                ..Default::default()
            },
        )?;
        self.get_task_decision_requests(&request.task_id)?
            .into_iter()
            .find(|candidate| candidate.id == request.id)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "decision request `{request_id}` vanished after its answer was applied"
                ))
            })
    }

    /// The gate's own write for one allowed answer, as the human. The
    /// caller has already refused every stale or disallowed answer.
    fn apply_decision_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        note: Option<&str>,
        actor_label: &str,
        provenance: Option<&str>,
    ) -> Result<(), OrbitError> {
        let task_id = request.task_id.as_str();
        let via = provenance.unwrap_or("operator surface");
        let chosen = request
            .answers
            .iter()
            .find(|answer| answer.id == answer_id)
            .map(|answer| answer.label.as_str())
            .unwrap_or(answer_id);
        match request.gate {
            DecisionGate::BlockTriage => {
                if answer_id == "dismiss" {
                    let comment = decision_comment(
                        request,
                        answer_id,
                        chosen,
                        via,
                        note,
                        "The task stays blocked for this episode.",
                    );
                    self.update_task_as_human(
                        task_id,
                        TaskUpdateParams {
                            comment: Some(comment),
                            ..Default::default()
                        },
                        actor_label.to_string(),
                    )?;
                    return Ok(());
                }
                let mut consequence = "The task returns to backlog.".to_string();
                if let Some(replacement) = self.triage_replacement_text(task_id, answer_id)? {
                    consequence.push_str(&format!("\nReplacement text:\n{replacement}"));
                }
                let comment = decision_comment(request, answer_id, chosen, via, note, &consequence);
                self.update_task_as_human(
                    task_id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        status: Some(TaskStatus::Backlog),
                        ..Default::default()
                    },
                    actor_label.to_string(),
                )?;
                Ok(())
            }
            DecisionGate::PilotAdmission => {
                let verdict = match answer_id {
                    value if value == PilotAdmissionVerdict::ApproveAnyway.as_str() => {
                        "approve-anyway"
                    }
                    value if value == PilotAdmissionVerdict::Clear.as_str() => "clear",
                    value if value == PilotAdmissionVerdict::Evaluated.as_str() => "evaluated",
                    other => {
                        return Err(OrbitError::InvalidInput(format!(
                            "answer_not_allowed: `{other}` is not a pilot-admission verdict; nothing was applied"
                        )));
                    }
                };
                // The trusted human path records the typed resolution from
                // this comment's header; the lines after it are its evidence.
                let evidence =
                    note.unwrap_or("The operator answered the decision request for this hold.");
                let comment = format!(
                    "task-pilot-admission: {verdict}\n{evidence}\nAnswered decision request {} via {via}.",
                    request.id
                );
                self.update_task_as_human(
                    task_id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        ..Default::default()
                    },
                    actor_label.to_string(),
                )?;
                Ok(())
            }
            DecisionGate::ProposedApproval | DecisionGate::ReviewApproval => {
                let comment = decision_comment(request, answer_id, chosen, via, note, "");
                match answer_id {
                    "approve" => {
                        self.approve_task_screened(
                            task_id,
                            note.map(str::to_string),
                            Some(comment),
                            None,
                            None,
                        )?;
                        Ok(())
                    }
                    "reject" => {
                        let reason = note.map(str::to_string).unwrap_or_else(|| {
                            format!("rejected via decision request {}", request.id)
                        });
                        self.reject_task(task_id, reason, Some(comment))?;
                        Ok(())
                    }
                    other => Err(OrbitError::InvalidInput(format!(
                        "answer_not_allowed: `{other}` is not an approval answer; nothing was applied"
                    ))),
                }
            }
        }
    }

    /// The chosen option's replacement text from the triage proposal
    /// comment on `task_id`, when `answer_id` is one of its option numbers
    /// and the comment is still there. The request record carries only
    /// answer ids, labels and consequences, so the ready-to-apply text is
    /// quoted back from the proposal that raised the request.
    fn triage_replacement_text(
        &self,
        task_id: &str,
        answer_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        let Ok(option) = answer_id.parse::<usize>() else {
            return Ok(None);
        };
        if option == 0 {
            return Ok(None);
        }
        let comments = self.get_task_comments(task_id)?;
        let Some(proposal) = comments.iter().rev().find(|comment| {
            comment
                .message
                .starts_with("blocked_task_triage disposition=decision")
        }) else {
            return Ok(None);
        };
        let marker = format!("\n\nOption {option}: ");
        let Some(start) = proposal.message.find(&marker) else {
            return Ok(None);
        };
        let section = &proposal.message[start + marker.len()..];
        let Some((_, replacement)) = section.split_once("\nReplacement text:\n") else {
            return Ok(None);
        };
        let end = [
            "\n\nOption ",
            "\n\nRecommended:",
            "\nNothing else on this task",
        ]
        .iter()
        .filter_map(|needle| replacement.find(needle))
        .min()
        .unwrap_or(replacement.len());
        let text = replacement[..end].trim();
        Ok((!text.is_empty()).then(|| text.to_string()))
    }
}

/// The human comment an applied answer records beside its own effect.
fn decision_comment(
    request: &DecisionRequest,
    answer_id: &str,
    label: &str,
    via: &str,
    note: Option<&str>,
    consequence: &str,
) -> String {
    let mut comment = format!(
        "Decision answer: {label} (`{answer_id}`) for decision request {} via {via}.",
        request.id
    );
    if !consequence.is_empty() {
        comment.push('\n');
        comment.push_str(consequence);
    }
    if let Some(note) = note {
        comment.push('\n');
        comment.push_str(note);
    }
    comment
}
