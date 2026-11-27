//! The human answer entry and its per-gate apply transaction.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DecisionAction, DecisionGate, DecisionRequest, DecisionResolution,
    PilotAdmissionOperatorRecord, PilotAdmissionVerdict, TaskHistoryEntry, TaskStatus,
};

#[cfg(feature = "test-support")]
use super::answer_checkpoint;
use super::{
    CANDIDATE_DISCARDED_NOTE, DecisionAnswerEffects, decision_comment, ensure_request_answerable,
};
use crate::OrbitRuntime;
use crate::application::task::TaskRecordUpdateParams;
use crate::application::task::params::TaskUpdateParams;
use crate::application::task::{
    PilotAdmissionHold, TaskCorpusPort, TaskJobPort, TaskRecords, TaskResidentPort, TriageTarget,
};

impl OrbitRuntime {
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
        if is_automation_actor(&actor_label)
            || actor_label.trim() == orbit_types::task::PRECEDENT_ACTOR
        {
            let who = actor_label.trim();
            let why = if who == orbit_types::task::PRECEDENT_ACTOR {
                "precedent answers are recorded only from an operator-approved corpus rule"
            } else {
                "is automation"
            };
            return Err(OrbitError::decision_refused(
                DecisionRefusalCode::CallerNotAllowed,
                format!(
                    "decision request `{request_id}` is answered by a human; `{who}` {why}, so nothing was applied"
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
                let chosen_option = request
                    .answers
                    .iter()
                    .find(|answer| answer.id == answer_id)
                    .and_then(|answer| answer.option.as_ref());
                let lands_handoff =
                    chosen_option.is_some_and(|option| option.action == Some(DecisionAction::Land));
                // A blocked task's reject option commits its status and this
                // answer's resolution in one store update, so the resolution
                // is composed before the apply step instead of after it.
                let rejects_blocked = request.gate == DecisionGate::BlockTriage
                    && chosen_option
                        .is_some_and(|option| option.action == Some(DecisionAction::Reject));
                let compose = || -> Result<_, OrbitError> {
                    let now = chrono::Utc::now();
                    let resolution = DecisionResolution {
                        request_id: request.id.clone(),
                        answer_id: answer_id.to_string(),
                        answered_by: actor_label.clone(),
                        provenance: provenance.clone(),
                        note: note.clone(),
                        answered_at: now,
                        precedent: None,
                        model: None,
                        confirmation: None,
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
                        surface: None,
                        caller: None,
                    };
                    let corpus_append = crate::app::port::TaskCorpus { rt: self }
                        .task_corpus_append_for_decision(&request, &resolution)?;
                    Ok((resolution, entry, corpus_append))
                };
                let early = if rejects_blocked {
                    let (_, entry, corpus_append) = compose()?;
                    Some(
                        std::iter::once(entry)
                            .chain(corpus_append)
                            .collect::<Vec<_>>(),
                    )
                } else {
                    None
                };
                let effects = if lands_handoff {
                    DecisionAnswerEffects::default()
                } else {
                    self.apply_decision_answer(
                        &request,
                        answer_id,
                        note.as_deref(),
                        &actor_label,
                        provenance.as_deref(),
                        early.clone().unwrap_or_default(),
                    )?
                };
                deferred = effects.deferred_approval;
                if early.is_none() {
                    let (resolution, entry, corpus_append) = compose()?;
                    if lands_handoff {
                        self.land_handoff_decision(&request, &resolution, entry, corpus_append)?;
                    } else {
                        self.stores().task_records().update(
                            &task_id,
                            TaskRecordUpdateParams {
                                actor: actor_label.clone(),
                                append_history: std::iter::once(entry)
                                    .chain(corpus_append)
                                    .collect(),
                                ..Default::default()
                            },
                        )?;
                    }
                }
                if effects.requirements_rewritten {
                    self.request_pilot_after_decision_rewrite(&task_id, &actor_label);
                }
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
    /// stale or disallowed answer. The returned effects include any
    /// approval work deferred until that lock is released, and whether
    /// a rewrite may need a pilot once the resolution is recorded.
    /// `resolution_history` is
    /// non-empty only for a blocked task's reject option, which writes it in
    /// its own status update; every other answer's caller records it after.
    fn apply_decision_answer(
        &self,
        request: &DecisionRequest,
        answer_id: &str,
        note: Option<&str>,
        actor_label: &str,
        provenance: Option<&str>,
        resolution_history: Vec<TaskHistoryEntry>,
    ) -> Result<DecisionAnswerEffects, OrbitError> {
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
            return self.apply_option_answer(
                request,
                answer,
                option,
                (note, actor_label, via),
                resolution_history,
            );
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
                    return Ok(DecisionAnswerEffects::default());
                }
                let edit = self.triage_option_edit(task_id, answer_id)?;
                let consequence = format!(
                    "The task's {} is replaced and it returns to backlog.\nReplacement text:\n{}",
                    edit.target.as_str(),
                    edit.replacement
                );
                let mut params = TaskUpdateParams {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                };
                match edit.target {
                    TriageTarget::AcceptanceCriteria => {
                        params.acceptance_criteria = Some(edit.criteria);
                    }
                    TriageTarget::Description => params.description = Some(edit.replacement),
                }
                let task = self.get_task(task_id)?;
                params.discard_candidate = self.rewrite_strands_candidate(
                    &task,
                    params.description.as_deref(),
                    params.acceptance_criteria.as_deref(),
                )?;
                let consequence = if params.discard_candidate {
                    format!("{consequence}{CANDIDATE_DISCARDED_NOTE}")
                } else {
                    consequence
                };
                params.comment = Some(decision_comment(
                    request,
                    answer_id,
                    chosen,
                    via,
                    note,
                    &consequence,
                ));
                self.update_task_as_human(task_id, params, actor_label.to_string())?;
                Ok(DecisionAnswerEffects {
                    requirements_rewritten: true,
                    ..Default::default()
                })
            }
            DecisionGate::PilotAdmission => {
                let verdict = match answer_id {
                    value if value == PilotAdmissionVerdict::ApproveAnyway.as_str() => {
                        PilotAdmissionVerdict::ApproveAnyway
                    }
                    value if value == PilotAdmissionVerdict::Clear.as_str() => {
                        PilotAdmissionVerdict::Clear
                    }
                    value if value == PilotAdmissionVerdict::Evaluated.as_str() => {
                        PilotAdmissionVerdict::Evaluated
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
                // `evaluated`, and older requests exist for stale holds.
                // Refuse before the comment: a successful apply is what
                // appends `decision_answered`, and these verdicts record no
                // resolution for the hold.
                if let Some(hold) = self.pilot_admission_hold(task_id)?
                    && !hold.accepts_decision_answer(verdict)
                {
                    let message = if matches!(hold, PilotAdmissionHold::StaleAssessment(_)) {
                        format!(
                            "`{answer_id}` does not resolve a stale pilot assessment, which needs a new pilot; nothing was applied"
                        )
                    } else {
                        format!(
                            "`{answer_id}` does not resolve a duplicate, already-landed or unresolved-footprint pilot hold; nothing was applied and the request stays open"
                        )
                    };
                    return Err(OrbitError::decision_refused(
                        DecisionRefusalCode::AnswerNotApplicable,
                        message,
                    ));
                }
                // The trusted human path records the typed resolution from
                // this comment's header; the lines after it are its evidence.
                let evidence =
                    note.unwrap_or("The operator answered the decision request for this hold.");
                let record = PilotAdmissionOperatorRecord::from_verdict(verdict, evidence)
                    .map_err(|_| {
                        OrbitError::Execution(format!(
                            "pilot-admission verdict `{answer_id}` could not be recorded"
                        ))
                    })?;
                let comment = format!(
                    "{}\nAnswered decision request {} via {via}.",
                    record.comment(),
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
                Ok(DecisionAnswerEffects::default())
            }
            DecisionGate::AgentRequest => {
                let task = self.get_task(task_id)?;
                if !self.is_human_crew(task.crew.as_deref()) {
                    return Err(OrbitError::decision_refused(
                        DecisionRefusalCode::AnswerNotApplicable,
                        format!(
                            "task {task_id} is no longer assigned to a human crew, so its decision cannot complete it; nothing was applied and the request stays open"
                        ),
                    ));
                }
                let consequence = request
                    .answer(answer_id)
                    .map(|answer| format!("Consequence: {}", answer.consequence))
                    .unwrap_or_default();
                let comment = decision_comment(request, answer_id, chosen, via, note, &consequence);
                self.update_task_as_human(
                    task_id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        status: Some(TaskStatus::Done),
                        ..Default::default()
                    },
                    actor_label.to_string(),
                )?;
                Ok(DecisionAnswerEffects::default())
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
                        Ok(DecisionAnswerEffects {
                            deferred_approval: Some(effects),
                            ..Default::default()
                        })
                    }
                    "reject" => {
                        let reason = note.map(str::to_string).unwrap_or_else(|| {
                            format!("rejected via decision request {}", request.id)
                        });
                        self.reject_task(task_id, reason, Some(comment))?;
                        Ok(DecisionAnswerEffects::default())
                    }
                    other => Err(OrbitError::decision_refused(
                        DecisionRefusalCode::AnswerNotAllowed,
                        format!("`{other}` is not an approval answer; nothing was applied"),
                    )),
                }
            }
            // [ORB-22808] Decision assistance was removed: a review request
            // recorded before then can no longer be answered.
            DecisionGate::ModelAnswerReview => Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!(
                    "decision request `{}` reviews a model answer, and decision assistance was \
                     removed; reverse the answered request with `orbit decision reverse` \
                     instead. Nothing was applied",
                    request.id
                ),
            )),
            DecisionGate::ResidentAuthority => {
                crate::app::port::TaskResident(self).apply_resident_authority_decision_answer(
                    request,
                    answer_id,
                    actor_label,
                )?;
                Ok(DecisionAnswerEffects::default())
            }
            DecisionGate::ScriptGrant => {
                self.answer_script_grant_decision(request, answer_id, actor_label, via)?;
                Ok(DecisionAnswerEffects::default())
            }
        }
    }
}
