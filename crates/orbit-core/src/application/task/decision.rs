//! Listing, showing and answering human decision requests.
//!
//! Requests are raised on held work by block triage and pilot admission,
//! and on proposed and review tasks by the decision digest in
//! `decision_channel` (see `orbit_types::task::DecisionRequest`). They
//! live in task history, so listing scans task histories — the shape the
//! block readout already has.
//!
//! [`OrbitRuntime::answer_decision_request_as_human`] is the core apply
//! step every answering surface shares: the operator CLI, the dashboard,
//! and later the delegated channel that submits a human's answer to core.
//! It refuses a stale answer (superseded, expired, or an answer outside
//! the request's allowed set) with the design's named refusals and writes
//! nothing. An allowed pilot answer that cannot resolve the current
//! duplicate or already-landed hold is refused the same way, before any
//! comment. Every other allowed answer lands through the human funnel
//! (`update_task_as_human`, or `approve_task_screened` / `reject_task` for
//! approvals) and only then records `decision_answered`, which is what
//! makes the request read `answered`. A proposed approval is screened
//! first (`ensure_proposal_admissible`): work a pilot hold or a missing
//! prepared footprint would keep out of the backlog is refused the same
//! way, before any write.

use orbit_common::OrbitError;
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DecisionGate, DecisionRequest, DecisionRequestState,
    DecisionResolution, PilotAdmissionVerdict, ReadinessGap, ReadinessStage, TaskHistoryEntry,
    TaskStatus, readiness_gaps,
};
use serde_json::{Value, json};

use super::TaskRecordUpdateParams;
use super::params::TaskUpdateParams;
use crate::OrbitRuntime;
use crate::application::task::{PilotAdmissionHold, TaskRecords, TriageTarget};

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
                let edit = self.triage_option_edit(task_id, answer_id)?;
                let consequence = format!(
                    "The task's {} is replaced and it returns to backlog.\nReplacement text:\n{}",
                    edit.target.as_str(),
                    edit.replacement
                );
                let comment = decision_comment(request, answer_id, chosen, via, note, &consequence);
                let mut params = TaskUpdateParams {
                    comment: Some(comment),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                };
                match edit.target {
                    TriageTarget::AcceptanceCriteria => {
                        params.acceptance_criteria = Some(edit.criteria);
                    }
                    TriageTarget::Description => params.description = Some(edit.replacement),
                }
                self.update_task_as_human(task_id, params, actor_label.to_string())?;
                Ok(())
            }
            DecisionGate::PilotAdmission => {
                let (verdict, header) = match answer_id {
                    value if value == PilotAdmissionVerdict::ApproveAnyway.as_str() => {
                        (PilotAdmissionVerdict::ApproveAnyway, "approve-anyway")
                    }
                    value if value == PilotAdmissionVerdict::Clear.as_str() => {
                        (PilotAdmissionVerdict::Clear, "clear")
                    }
                    value if value == PilotAdmissionVerdict::Evaluated.as_str() => {
                        (PilotAdmissionVerdict::Evaluated, "evaluated")
                    }
                    other => {
                        return Err(OrbitError::InvalidInput(format!(
                            "answer_not_allowed: `{other}` is not a pilot-admission verdict; nothing was applied"
                        )));
                    }
                };
                // Requests raised before the offer was hold-aware still list
                // `evaluated`. Refuse it before the comment: a successful
                // apply is what appends `decision_answered`, and this verdict
                // records no resolution for a duplicate or already-landed hold.
                if let Some(hold) = self.pilot_admission_hold(task_id)?
                    && !hold.accepts_decision_answer(verdict)
                {
                    return Err(OrbitError::InvalidInput(format!(
                        "answer_not_applicable: `{answer_id}` does not resolve a duplicate or already-landed pilot hold; nothing was applied and the request stays open"
                    )));
                }
                // The trusted human path records the typed resolution from
                // this comment's header; the lines after it are its evidence.
                let evidence =
                    note.unwrap_or("The operator answered the decision request for this hold.");
                let comment = format!(
                    "task-pilot-admission: {header}\n{evidence}\nAnswered decision request {} via {via}.",
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
                        if request.gate == DecisionGate::ProposedApproval {
                            self.ensure_proposal_admissible(task_id)?;
                        }
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

    /// The task-pilot preparation and admission checks a digest-raised
    /// proposal approval runs before promotion, unlike the advisory
    /// screened approval an operator surface runs beside its warnings.
    ///
    /// Refuses, before anything is written, while a pilot admission
    /// hold or a `verified_no_diff` finding stands, or while a blocking
    /// readiness gap stands at the proposed stage: no declared footprint
    /// without the no-diff disposition, or an unassessed complexity.
    /// The task stays proposed and the request stays open, so a later
    /// answer can still land once the pilot prepares the task or a
    /// verdict resolves the hold.
    fn ensure_proposal_admissible(&self, task_id: &str) -> Result<(), OrbitError> {
        let task = self.get_task(task_id)?;
        if task.status != TaskStatus::Proposed {
            // The approval path itself reports the wrong status.
            return Ok(());
        }
        if let Some(hold) = self.pilot_admission_hold(task_id)? {
            return Err(OrbitError::InvalidInput(format!(
                "answer_not_applicable: task-pilot holds this proposal ({}) and an approval does not resolve that hold; nothing was applied, the task stays proposed and the request stays open",
                pilot_hold_classification(&hold)
            )));
        }
        if self
            .verified_no_diff(task_id, task.status, &task.tags)?
            .is_some()
        {
            return Err(OrbitError::InvalidInput(
                "answer_not_applicable: task-pilot verified this proposal needs no change (pilot_verified_no_diff), so it is held for that disposition instead of promotion; nothing was applied, the task stays proposed and the request stays open"
                    .to_string(),
            ));
        }
        let gaps: Vec<ReadinessGap> = readiness_gaps(
            ReadinessStage::Proposed,
            &task.tags,
            &task.context_files,
            task.complexity,
        )
        .into_iter()
        .filter(ReadinessGap::is_blocking)
        .collect();
        if !gaps.is_empty() {
            let detail = gaps
                .iter()
                .map(|gap| format!("{} (fix: {})", gap.code.as_str(), gap.fix))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(OrbitError::InvalidInput(format!(
                "answer_not_applicable: this proposal is not prepared for the backlog ({detail}); nothing was applied, the task stays proposed and the request stays open"
            )));
        }
        Ok(())
    }

    /// The edit the chosen option of the triage proposal on `task_id`
    /// makes, read back from the proposal comment that raised the request
    /// (the request record carries only answer ids, labels and
    /// consequences). Refuses, before anything is written, an option whose
    /// target or replacement cannot be determined or that would leave the
    /// task as it is: returning such work to backlog would re-admit it with
    /// its blocker unresolved.
    fn triage_option_edit(&self, task_id: &str, answer_id: &str) -> Result<TriageEdit, OrbitError> {
        let refuse = |reason: &str| {
            OrbitError::InvalidInput(format!(
                "answer_not_applicable: option `{answer_id}` cannot be applied ({reason}); nothing was applied and the task stays blocked"
            ))
        };
        let option = answer_id
            .parse::<usize>()
            .ok()
            .filter(|option| *option > 0)
            .ok_or_else(|| refuse("it is not an option number"))?;
        let comments = self.get_task_comments(task_id)?;
        let proposal = comments
            .iter()
            .rev()
            .find(|comment| {
                comment
                    .message
                    .starts_with("blocked_task_triage disposition=decision")
            })
            .ok_or_else(|| refuse("the proposal comment is gone"))?;
        let marker = format!("\n\nOption {option}: ");
        let section = proposal
            .message
            .find(&marker)
            .map(|start| &proposal.message[start + marker.len()..])
            .ok_or_else(|| refuse("the proposal comment does not list it"))?;
        let (head, replacement) = section
            .split_once("\nReplacement text:\n")
            .ok_or_else(|| refuse("the proposal comment records no replacement text"))?;
        let target = head
            .lines()
            .last()
            .and_then(|line| line.strip_prefix("Applies to: "))
            .and_then(TriageTarget::parse)
            .ok_or_else(|| refuse("the proposal names no field its replacement rewrites"))?;
        let end = [
            "\n\nOption ",
            "\n\nRecommended:",
            "\nNothing else on this task",
        ]
        .iter()
        .filter_map(|needle| replacement.find(needle))
        .min()
        .unwrap_or(replacement.len());
        let replacement = replacement[..end].trim();
        if replacement.is_empty() {
            return Err(refuse("its replacement text is empty"));
        }
        if replacement.contains("[REDACTED") {
            return Err(refuse("its replacement text was redacted"));
        }
        let task = self.get_task(task_id)?;
        let criteria: Vec<String> = replacement
            .lines()
            .map(|line| {
                let line = line.trim();
                line.strip_prefix("- ")
                    .or_else(|| line.strip_prefix("* "))
                    .unwrap_or(line)
                    .trim()
                    .to_string()
            })
            .filter(|line| !line.is_empty())
            .collect();
        let unchanged = match target {
            TriageTarget::AcceptanceCriteria => criteria == task.acceptance_criteria,
            TriageTarget::Description => replacement == task.description.trim(),
        };
        if unchanged {
            return Err(refuse(&format!(
                "its replacement leaves the task's {} unchanged",
                target.as_str()
            )));
        }
        Ok(TriageEdit {
            target,
            replacement: replacement.to_string(),
            criteria,
        })
    }
}

/// The hold classification a refusal names, in the vocabulary drain
/// approval and admission already report.
fn pilot_hold_classification(hold: &PilotAdmissionHold) -> &'static str {
    match hold {
        PilotAdmissionHold::Duplicate => "pilot_duplicate",
        PilotAdmissionHold::AlreadyLanded => "pilot_already_landed",
        PilotAdmissionHold::OperatorValidation(_) => "operator_validation",
        PilotAdmissionHold::HostOperational(_) => "host_operational",
        PilotAdmissionHold::NativeOs(_) => "native_os",
        PilotAdmissionHold::UnverifiedFinding(_) => "unverified_review_finding",
    }
}

/// One triage option's edit to the held task.
struct TriageEdit {
    target: TriageTarget,
    /// The option's replacement text as the proposal recorded it.
    replacement: String,
    /// `replacement` as a criteria list; what an acceptance-criteria target
    /// writes.
    criteria: Vec<String>,
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
