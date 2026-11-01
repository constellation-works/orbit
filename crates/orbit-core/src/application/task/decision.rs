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
//! nothing. The request is judged on a fresh read under the task's write
//! lock, which the gate's effect and the `decision_answered` record share,
//! so a task edit cannot land between the check and the effect. An allowed
//! pilot answer that cannot resolve the current duplicate or already-landed
//! hold is refused the same way, before any comment. Every other allowed answer lands through the human funnel
//! (`update_task_as_human`, or the guarded approval / `reject_task` for
//! approvals) and only then records `decision_answered`, which is what
//! makes the request read `answered`. A proposed approval is screened
//! first (`ensure_proposal_admissible`): work a pilot hold or a missing
//! prepared footprint would keep out of the backlog is refused the same
//! way, before any write.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DecisionAction, DecisionAnswer, DecisionGate, DecisionOption,
    DecisionRequest, DecisionRequestState, DecisionResolution, PilotAdmissionVerdict, ReadinessGap,
    ReadinessStage, TaskHistoryEntry, TaskRelation, TaskRelationType, TaskStatus, readiness_gaps,
};
use serde_json::{Value, json};

use super::TaskRecordUpdateParams;
use super::params::{TaskAddParams, TaskUpdateParams};
use super::transitions::DeferredApprovalEffects;
use crate::OrbitRuntime;
use crate::application::task::{PilotAdmissionHold, TaskRecords, TriageTarget};

/// Decision requests readable in a workspace scan, with corrupt bundles
/// identified separately so a partial result never looks complete.
#[derive(Debug, Default)]
pub struct DecisionRequestListing {
    /// Matching requests, oldest first.
    pub requests: Vec<DecisionRequest>,
    /// Per-task bundle corruption encountered during the scan.
    pub warnings: Vec<String>,
}

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
/// request id, its gate, the answer ids a surface may offer, expiry, and the
/// decision card: the short `summary`, the `human_action` of a request only a
/// person outside Orbit can resolve, and `options` (each answer's id, label,
/// consequence and whether it is the `recommended` one) so a surface renders
/// one button per option without a second read.
pub(crate) fn open_decision_json(request: &DecisionRequest) -> Value {
    let recommended = request.recommended.as_deref();
    let mut entry = json!({
        "id": request.id,
        "gate": request.gate.as_str(),
        "answers": request
            .answers
            .iter()
            .map(|answer| answer.id.clone())
            .collect::<Vec<_>>(),
        "options": request
            .answers
            .iter()
            .map(|answer| {
                json!({
                    "id": answer.id,
                    "label": answer.label,
                    "consequence": answer.consequence,
                    "recommended": recommended == Some(answer.id.as_str()),
                })
            })
            .collect::<Vec<_>>(),
        "expires_at": request.expires_at.to_rfc3339(),
    });
    if let Some(summary) = &request.summary {
        entry["summary"] = json!(summary);
    }
    if let Some(action) = &request.human_action {
        entry["human_action"] = json!(action);
    }
    entry
}

impl OrbitRuntime {
    /// Decision requests across the workspace, oldest first. Open requests
    /// only unless `include_closed`; `gate` and `task_id` narrow the list.
    /// Strict: any unreadable bundle fails the call. Listing surfaces use
    /// [`Self::list_decision_requests_with_warnings`] for partial results.
    pub fn list_decision_requests(
        &self,
        gate: Option<DecisionGate>,
        task_id: Option<&str>,
        include_closed: bool,
    ) -> Result<Vec<DecisionRequest>, OrbitError> {
        Ok(self
            .scan_decision_requests(gate, task_id, include_closed, false)?
            .requests)
    }

    /// List readable requests and warn for corrupt bundles in a workspace
    /// scan. An explicit `task_id` stays strict, as do non-corruption errors.
    pub fn list_decision_requests_with_warnings(
        &self,
        gate: Option<DecisionGate>,
        task_id: Option<&str>,
        include_closed: bool,
    ) -> Result<DecisionRequestListing, OrbitError> {
        self.scan_decision_requests(gate, task_id, include_closed, task_id.is_none())
    }

    fn scan_decision_requests(
        &self,
        gate: Option<DecisionGate>,
        task_id: Option<&str>,
        include_closed: bool,
        tolerate_corrupt: bool,
    ) -> Result<DecisionRequestListing, OrbitError> {
        let ids: Vec<String> = match task_id {
            Some(task_id) => vec![task_id.to_string()],
            None => self
                .list_task_metadata()?
                .into_iter()
                .map(|task| task.id)
                .collect(),
        };
        let mut listing = DecisionRequestListing::default();
        for id in &ids {
            let read = self.get_task_decision_requests(id);
            let requests = if tolerate_corrupt {
                let Some(requests) =
                    super::listing::task_bundle_scan_result(read, &mut listing.warnings)?
                else {
                    continue;
                };
                requests
            } else {
                read?
            };
            for request in requests {
                if gate.is_some_and(|gate| gate != request.gate) {
                    continue;
                }
                if !include_closed && request.state != DecisionRequestState::Open {
                    continue;
                }
                listing.requests.push(request);
            }
        }
        listing
            .requests
            .sort_by(|a, b| (a.raised_at, &a.id).cmp(&(b.raised_at, &b.id)));
        Ok(listing)
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
        if is_automation_actor(&actor_label) {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::CallerNotAllowed,
                format!(
                    "decision request `{request_id}` is answered by a human; `{}` is automation, so nothing was applied",
                    actor_label.trim()
                ),
            ));
        }
        let not_found = || {
            OrbitError::decision_refused(
                DecisionRefusalCode::RequestNotFound,
                format!("no decision request `{request_id}` in this workspace"),
            )
        };
        // The workspace scan only locates the request's task. Its state,
        // binding, expiry and allowed answers are judged again below on a
        // fresh read under that task's write lock, which the gate's effect
        // and the `decision_answered` record share: a task edit that lands
        // after this lookup supersedes the request before anything applies.
        let task_id = self
            .get_decision_request(request_id)?
            .ok_or_else(not_found)?
            .task_id;
        #[cfg(feature = "test-support")]
        answer_checkpoint::notify();
        let note = note
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let mut answered = None;
        let mut deferred = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&task_id, &mut || {
                let request = self
                    .get_task_decision_requests(&task_id)?
                    .into_iter()
                    .find(|candidate| candidate.id == request_id)
                    .ok_or_else(not_found)?;
                ensure_request_answerable(&request, answer_id)?;
                deferred = self.apply_decision_answer(
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
                let entry = TaskHistoryEntry {
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
                    &task_id,
                    TaskRecordUpdateParams {
                        actor: actor_label.clone(),
                        append_history: vec![entry],
                        ..Default::default()
                    },
                )?;
                answered = self
                    .get_task_decision_requests(&task_id)?
                    .into_iter()
                    .find(|candidate| candidate.id == request.id);
                Ok(())
            })?;
        // A review approval's effects on other records run only once this
        // task's lock is released.
        if let Some(effects) = deferred {
            self.run_deferred_approval_effects(effects);
        }
        answered.ok_or_else(|| {
            OrbitError::Execution(format!(
                "decision request `{request_id}` vanished after its answer was applied"
            ))
        })
    }

    /// The gate's own write for one allowed answer, as the human. The
    /// caller holds the task's write lock and has already refused every
    /// stale or disallowed answer. An approval returns the effects it
    /// deferred until that lock is released.
    fn apply_decision_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        note: Option<&str>,
        actor_label: &str,
        provenance: Option<&str>,
    ) -> Result<Option<DeferredApprovalEffects>, OrbitError> {
        let task_id = request.task_id.as_str();
        let via = provenance.unwrap_or("operator surface");
        let chosen = request
            .answers
            .iter()
            .find(|answer| answer.id == answer_id)
            .map(|answer| answer.label.as_str())
            .unwrap_or(answer_id);
        if matches!(
            request.gate,
            DecisionGate::BlockTriage | DecisionGate::ProposedApproval
        ) && let Some(answer) = request.answers.iter().find(|answer| answer.id == answer_id)
            && let Some(option) = &answer.option
        {
            return self.apply_option_answer(request, answer, option, note, actor_label, via);
        }
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
                    return Ok(None);
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
                Ok(None)
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
                        return Err(OrbitError::decision_refused(
                            DecisionRefusalCode::AnswerNotAllowed,
                            format!(
                                "`{other}` is not a pilot-admission verdict; nothing was applied"
                            ),
                        ));
                    }
                };
                // Requests raised before the offer was hold-aware still list
                // `evaluated`. Refuse it before the comment: a successful
                // apply is what appends `decision_answered`, and this verdict
                // records no resolution for a duplicate or already-landed hold.
                if let Some(hold) = self.pilot_admission_hold(task_id)?
                    && !hold.accepts_decision_answer(verdict)
                {
                    return Err(OrbitError::decision_refused(
                        DecisionRefusalCode::AnswerNotApplicable,
                        format!(
                            "`{answer_id}` does not resolve a duplicate or already-landed pilot hold; nothing was applied and the request stays open"
                        ),
                    ));
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
                Ok(None)
            }
            DecisionGate::ProposedApproval | DecisionGate::ReviewApproval => {
                let comment = decision_comment(request, answer_id, chosen, via, note, "");
                match answer_id {
                    "approve" => {
                        if request.gate == DecisionGate::ProposedApproval {
                            self.ensure_proposal_admissible(task_id)?;
                        }
                        let (_, effects) = self.approve_task_guarded_deferring_effects(
                            task_id,
                            note.map(str::to_string),
                            Some(comment),
                        )?;
                        Ok(Some(effects))
                    }
                    "reject" => {
                        let reason = note.map(str::to_string).unwrap_or_else(|| {
                            format!("rejected via decision request {}", request.id)
                        });
                        self.reject_task(task_id, reason, Some(comment))?;
                        Ok(None)
                    }
                    other => Err(OrbitError::decision_refused(
                        DecisionRefusalCode::AnswerNotAllowed,
                        format!("`{other}` is not an approval answer; nothing was applied"),
                    )),
                }
            }
        }
    }

    /// Apply a one-click option as the human: its rewrite, then its action,
    /// then the move out of `blocked` (or the approval or re-pilot of a held
    /// proposed task), with one comment naming the option and the note. The
    /// caller holds the task's write lock and records the resolution, so the
    /// whole answer closes the request in one operation. Everything refusable
    /// is refused before the first write; a `split` files its follow-up
    /// first, so a failure there applies nothing to this task.
    fn apply_option_answer(
        &self,
        request: &DecisionRequest,
        answer: &DecisionAnswer,
        option: &DecisionOption,
        note: Option<&str>,
        actor_label: &str,
        via: &str,
    ) -> Result<Option<DeferredApprovalEffects>, OrbitError> {
        let task_id = request.task_id.as_str();
        let task = self.get_task(task_id)?;
        let refuse = |reason: &str| {
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "option `{}` cannot be applied ({reason}); nothing was applied and the request stays open",
                    answer.id
                ),
            )
        };
        let blocked = request.gate == DecisionGate::BlockTriage;
        let expected = if blocked {
            TaskStatus::Blocked
        } else {
            TaskStatus::Proposed
        };
        if task.status != expected {
            return Err(refuse(&format!(
                "the task is {}, not {expected}",
                task.status
            )));
        }

        let mut params = TaskUpdateParams::default();
        let mut applied = Vec::new();
        if let Some(rewrite) = &option.rewrite {
            if let Some(description) = &rewrite.description
                && description.trim() != task.description.trim()
            {
                params.description = Some(description.clone());
                applied.push("description rewritten");
            }
            if let Some(criteria) = &rewrite.acceptance_criteria
                && *criteria != task.acceptance_criteria
            {
                params.acceptance_criteria = Some(criteria.clone());
                applied.push("acceptance criteria rewritten");
            }
        }
        let rewrites = !applied.is_empty();
        let action_kind = option.action.as_ref().map(DecisionAction::kind);
        if option.action.is_none() && !rewrites {
            return Err(refuse("its rewrite leaves the task as it is"));
        }
        if !blocked && matches!(option.action, Some(DecisionAction::RePilot)) && !rewrites {
            return Err(refuse(
                "a proposed task is only re-assessed after a material edit, and this option rewrites nothing",
            ));
        }
        // A proposed task that the option approves must clear the same
        // screen an approval answer clears, before its rewrite lands.
        let approves = !blocked
            && matches!(
                option.action,
                Some(DecisionAction::Requeue | DecisionAction::ChangeCrew { .. })
                    | Some(DecisionAction::Split { .. })
            );
        if approves {
            self.ensure_proposal_admissible(task_id)?;
        }

        let mut consequence = format!("Consequence: {}", answer.consequence);
        if !applied.is_empty() {
            consequence.push_str(&format!("\nApplied: {}.", applied.join(", ")));
        }
        if let Some(kind) = action_kind {
            consequence.push_str(&format!("\nAction: {kind}."));
        }
        let comment = decision_comment(
            request,
            &answer.id,
            &format!("option {} — {}", answer.id, option.summary),
            via,
            note,
            &consequence,
        );
        params.comment = Some(comment.clone());
        let actor = actor_label.to_string();
        let backlog = if blocked {
            Some(TaskStatus::Backlog)
        } else {
            None
        };

        match &option.action {
            None | Some(DecisionAction::Requeue | DecisionAction::RePilot) => {
                params.status = backlog;
            }
            Some(DecisionAction::ChangeCrew { crew }) => {
                params.crew = Some(Some(crew.clone()));
                params.status = backlog;
            }
            Some(DecisionAction::Split {
                title,
                description,
                acceptance_criteria,
            }) => {
                let follow_up = self.add_task(TaskAddParams {
                    title: title.clone(),
                    description: description.clone(),
                    acceptance_criteria: acceptance_criteria.clone(),
                    relations: vec![TaskRelation {
                        relation_type: TaskRelationType::SpawnedFrom,
                        target: task.id.clone(),
                        regression: None,
                    }],
                    context_files: task.context_files.clone(),
                    priority: task.priority,
                    status: Some(TaskStatus::Proposed),
                    comment: Some(format!(
                        "Filed by decision request {} on {} (option {}: {}).",
                        request.id, task.id, answer.id, option.summary
                    )),
                    ..TaskAddParams::default()
                })?;
                params.comment = Some(format!(
                    "{comment}\nFollow-up task filed: {} (proposed).",
                    follow_up.id
                ));
                params.status = backlog;
            }
            Some(DecisionAction::Reject) => {
                let reason = note.map(str::to_string).unwrap_or_else(|| {
                    format!(
                        "rejected via option {} of decision request {}",
                        answer.id, request.id
                    )
                });
                self.reject_task(task_id, reason, Some(comment))?;
                return Ok(None);
            }
            Some(DecisionAction::Archive) => {
                params.status = Some(TaskStatus::Archived);
            }
        }
        self.update_task_as_human(task_id, params, actor)?;
        if approves {
            let (_, effects) = self.approve_task_guarded_deferring_effects(
                task_id,
                note.map(str::to_string),
                None,
            )?;
            return Ok(Some(effects));
        }
        Ok(None)
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
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "task-pilot holds this proposal ({}) and an approval does not resolve that hold; nothing was applied, the task stays proposed and the request stays open",
                    pilot_hold_classification(&hold)
                ),
            ));
        }
        if self
            .verified_no_diff(task_id, task.status, &task.tags)?
            .is_some()
        {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                "task-pilot verified this proposal needs no change (pilot_verified_no_diff), so it is held for that disposition instead of promotion; nothing was applied, the task stays proposed and the request stays open",
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
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "this proposal is not prepared for the backlog ({detail}); nothing was applied, the task stays proposed and the request stays open"
                ),
            ));
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
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "option `{answer_id}` cannot be applied ({reason}); nothing was applied and the task stays blocked"
                ),
            )
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

/// Refuse an answer to `request` as it reads now: one that is not open
/// (expired, answered or superseded) or whose answer it does not allow.
/// Nothing is written.
fn ensure_request_answerable(request: &DecisionRequest, answer_id: &str) -> Result<(), OrbitError> {
    let request_id = request.id.as_str();
    match request.state {
        DecisionRequestState::Open => {}
        DecisionRequestState::Expired => {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::RequestExpired,
                format!(
                    "decision request `{request_id}` expired at {}; nothing was applied",
                    request.expires_at.to_rfc3339()
                ),
            ));
        }
        DecisionRequestState::Answered => {
            let by = request
                .resolution
                .as_ref()
                .map_or("another answer", |resolution| {
                    resolution.answered_by.as_str()
                });
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::RequestSuperseded,
                format!(
                    "decision request `{request_id}` was already answered by {by}; nothing was applied"
                ),
            ));
        }
        DecisionRequestState::Superseded => {
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::RequestSuperseded,
                format!(
                    "decision request `{request_id}` no longer matches the hold on task {}; nothing was applied",
                    request.task_id
                ),
            ));
        }
    }
    if !request.allows(answer_id) {
        let allowed = request
            .answers
            .iter()
            .map(|answer| answer.id.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(OrbitError::decision_refused(
            DecisionRefusalCode::AnswerNotAllowed,
            format!(
                "`{answer_id}` is not an allowed answer for decision request `{request_id}` (allowed: {allowed}); nothing was applied"
            ),
        ));
    }
    Ok(())
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

/// A deterministic interleaving at the answer path: a hook runs after the
/// request lookup and before the task lock under which the answer is judged
/// and applied, so a test can land a competing task write in between.
#[cfg(feature = "test-support")]
pub(crate) mod answer_checkpoint {
    use std::cell::RefCell;

    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = RefCell::new(None);
    }

    /// Take the hook before running it, so a hook that answers again does
    /// not run itself a second time.
    pub(super) fn notify() {
        let hook = HOOK.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Run `operation` with `hook` called once, on this thread, between a
    /// decision answer's request lookup and its locked apply.
    pub fn with_decision_answer_checkpoint<T>(
        hook: impl FnOnce() + 'static,
        operation: impl FnOnce() -> T,
    ) -> T {
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                HOOK.with(|slot| *slot.borrow_mut() = None);
            }
        }
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        let _clear = Clear;
        operation()
    }
}
