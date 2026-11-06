//! Model answers to decision requests: the internal apply step, the
//! `model_answer_review` gate it raises, and that gate's confirm and veto.
//!
//! A model answer is written by the `model` automation actor, never as a
//! human label, so every guard that needs a human resolution refuses it. In
//! the same write as the answer, the apply step raises one
//! `model_answer_review` request on the task, bound to the answered request
//! and its resolution time. Only a human answers that review: `confirm`
//! records a `decision_model_confirmed` event and the corpus append with the
//! confirming human as the answerer; `veto` re-opens the original request and
//! restores the task status the answer recorded, through the same reversal
//! step `orbit decision reverse` uses. A model answer itself appends nothing
//! to the corpus outbox.
//!
//! Design: `docs/design/decision-assistance/specs/precedent-decisions.md`
//! (Recording, veto and feedback). Its caller is the precedent-decision
//! self-answer step (`precedent_decision::assist`), after its enablement,
//! tier, holdback, wait and threshold checks. A confirm or veto enters that
//! category's rolling demotion window.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::assistance::assistance_probability;
use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DECISION_MODEL_CONFIRMED_EVENT, DecisionBinding, DecisionGate,
    DecisionRequest, DecisionRequestState, DecisionResolution, DecisionRiskTier,
    MODEL_ANSWER_ACTOR, MODEL_ANSWER_CONFIRM, MODEL_ANSWER_VETO, ModelAnswerCitation,
    ModelAnswerConfirmation, TaskHistoryEntry, TaskStatus,
};

use super::super::TaskRecordUpdateParams;
use super::super::params::TaskUpdateParams;
use super::answering::{decision_comment, ensure_request_answerable};
use crate::OrbitRuntime;
use crate::application::task::{TaskPrecedentDecisionPort, TaskRecords};

/// What one applied model answer wrote: the answered request and the review
/// request raised beside it, each as it then reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAnswerApplied {
    /// The original request, now answered by the `model` actor.
    pub request: DecisionRequest,
    /// The open `model_answer_review` request for that answer.
    pub review: DecisionRequest,
}

/// One digest row for a model answer awaiting human review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelAnsweredDigestEntry {
    /// The open `model_answer_review` request.
    pub review: DecisionRequest,
    /// The model-answered request it reviews, with the model citation.
    pub request: DecisionRequest,
}

impl OrbitRuntime {
    /// Apply `answer_id` to the open request `request_id` as the `model`
    /// actor, recording `citation`, and raise its review request in the same
    /// write. Refused, with nothing written, unless the request is open, the
    /// answer is allowed, the request's tier is `routine` or `reversible`, the
    /// gate is not a model-answer review, and the citation's probabilities
    /// are valid and its category matches the request's.
    ///
    /// Only agent requests are applied in this release: their effect (the
    /// task completes) is restored exactly by a veto. Other gates are refused
    /// `answer_not_applicable`.
    pub(crate) fn apply_model_decision_answer(
        &self,
        request_id: &str,
        answer_id: &str,
        citation: ModelAnswerCitation,
    ) -> Result<ModelAnswerApplied, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        for (name, value) in [
            ("probability", citation.probability),
            ("threshold", citation.threshold),
            ("conflict_probability", citation.conflict_probability),
        ] {
            if !assistance_probability(value) {
                return Err(OrbitError::InvalidInput(format!(
                    "model answer `{name}` must be a probability in [0, 1]; nothing was applied"
                )));
            }
        }
        let not_found = || {
            OrbitError::decision_refused(
                DecisionRefusalCode::RequestNotFound,
                format!("no decision request `{request_id}` in this workspace"),
            )
        };
        let task_id = self
            .get_decision_request(request_id)?
            .ok_or_else(not_found)?
            .task_id;
        let workspace = self.workspace_id()?;
        let mut applied = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&task_id, &mut || {
                let requests = self.get_task_decision_requests(&task_id)?;
                let request = requests
                    .iter()
                    .find(|candidate| candidate.id == request_id)
                    .ok_or_else(not_found)?;
                let refuse = |code: DecisionRefusalCode, why: String| {
                    OrbitError::decision_refused(
                        code,
                        format!("decision request `{request_id}` {why}; nothing was applied"),
                    )
                };
                if request.gate == DecisionGate::ModelAnswerReview {
                    return Err(refuse(
                        DecisionRefusalCode::CallerNotAllowed,
                        "reviews a model answer, and only a human answers that review".into(),
                    ));
                }
                ensure_request_answerable(request, answer_id)?;
                if !matches!(
                    request.risk_tier,
                    DecisionRiskTier::Routine | DecisionRiskTier::Reversible
                ) {
                    return Err(refuse(
                        DecisionRefusalCode::AnswerNotApplicable,
                        format!(
                            "has risk tier `{}`; only a routine or reversible request may be answered by the model",
                            serde_json::to_value(request.risk_tier)
                                .ok()
                                .and_then(|value| value.as_str().map(str::to_string))
                                .unwrap_or_default()
                        ),
                    ));
                }
                if request.gate != DecisionGate::AgentRequest {
                    return Err(refuse(
                        DecisionRefusalCode::AnswerNotApplicable,
                        format!(
                            "is a `{}` request; the model answers only agent requests",
                            request.gate.as_str()
                        ),
                    ));
                }
                if let Some(category) = request.category.as_deref()
                    && category != citation.category
                {
                    return Err(refuse(
                        DecisionRefusalCode::AnswerNotApplicable,
                        format!(
                            "is in category `{category}`, not the cited `{}`",
                            citation.category
                        ),
                    ));
                }
                let task = self.get_task(&task_id)?;
                if !self.is_human_crew(task.crew.as_deref()) {
                    return Err(refuse(
                        DecisionRefusalCode::AnswerNotApplicable,
                        format!("is on task {task_id}, which is no longer assigned to a human crew"),
                    ));
                }
                let mut citation = citation.clone();
                citation.previous_status = Some(task.status);
                let chosen = request
                    .answer(answer_id)
                    .map(|answer| answer.label.clone())
                    .unwrap_or_else(|| answer_id.to_string());
                let now = chrono::Utc::now();
                let review = DecisionRequest::new(
                    task_id.clone(),
                    DecisionGate::ModelAnswerReview,
                    DecisionGate::model_answer_review_answers(),
                    DecisionBinding::ModelAnswer {
                        request_id: request.id.clone(),
                        answered_at: now,
                    },
                    MODEL_ANSWER_ACTOR.to_string(),
                    now,
                )
                .with_summary(
                    format!(
                        "The model answered request {} with `{answer_id}` ({chosen}) at probability {:.2}. Confirm it, or veto it to re-open the request.",
                        request.id, citation.probability
                    ),
                    None,
                )
                .with_category(&workspace);
                let comment = format!(
                    "Model answer: `{answer_id}` ({chosen}) for decision request {} at probability {:.2} (threshold {:.2}), citing {} precedent(s). Review request {} asks a human to confirm or veto; a veto re-opens {} and restores status {}.",
                    request.id,
                    citation.probability,
                    citation.threshold,
                    citation.cited_precedents.len(),
                    review.id,
                    request.id,
                    task.status,
                );
                self.update_task_as_decision_automation(
                    &task_id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        status: Some(TaskStatus::Done),
                        ..Default::default()
                    },
                    MODEL_ANSWER_ACTOR,
                )?;
                let resolution = DecisionResolution {
                    request_id: request.id.clone(),
                    answer_id: answer_id.to_string(),
                    answered_by: MODEL_ANSWER_ACTOR.to_string(),
                    provenance: Some(MODEL_ANSWER_ACTOR.to_string()),
                    note: None,
                    answered_at: now,
                    precedent: None,
                    model: Some(citation),
                    confirmation: None,
                };
                let answered = TaskHistoryEntry {
                    at: now,
                    by: MODEL_ANSWER_ACTOR.to_string(),
                    event: DECISION_ANSWERED_EVENT.to_string(),
                    note: serde_json::to_string(&resolution).ok(),
                    from_status: None,
                    to_status: None,
                    operation_id: Some(request.id.clone()),
                    decision: None,
                };
                // The answer and its review land in one write; a model answer
                // appends nothing to the corpus outbox.
                self.stores().task_records().update(
                    &task_id,
                    TaskRecordUpdateParams {
                        actor: MODEL_ANSWER_ACTOR.to_string(),
                        append_history: vec![answered, review.history_entry(MODEL_ANSWER_ACTOR)],
                        ..Default::default()
                    },
                )?;
                let requests = self.get_task_decision_requests(&task_id)?;
                let find = |id: &str| requests.iter().find(|candidate| candidate.id == id).cloned();
                applied = find(request_id).zip(find(&review.id));
                Ok(())
            })?;
        applied
            .map(|(request, review)| ModelAnswerApplied { request, review })
            .ok_or_else(|| {
                OrbitError::Execution(format!("model answer for `{request_id}` was not recorded"))
            })
    }

    /// The human's answer to a `model_answer_review` request. The caller
    /// holds the task write lock, has checked the review is open and the
    /// answer allowed, and records the review's own `decision_answered`
    /// afterwards.
    pub(super) fn apply_model_answer_review(
        &self,
        review: &DecisionRequest,
        answer_id: &str,
        note: Option<&str>,
        actor_label: &str,
        via: &str,
    ) -> Result<(), OrbitError> {
        let refuse = |why: &str| {
            OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotApplicable,
                format!("review request `{}` {why}; nothing was applied", review.id),
            )
        };
        let DecisionBinding::ModelAnswer {
            request_id,
            answered_at,
        } = &review.binding
        else {
            return Err(refuse("is not bound to a model answer"));
        };
        let original = self
            .get_task_decision_requests(&review.task_id)?
            .into_iter()
            .find(|candidate| &candidate.id == request_id)
            .ok_or_else(|| refuse("names a request that is gone"))?;
        let resolution = original
            .resolution
            .clone()
            .filter(|resolution| {
                original.state == DecisionRequestState::Answered
                    && resolution.answered_by == MODEL_ANSWER_ACTOR
                    && &resolution.answered_at == answered_at
            })
            .ok_or_else(|| refuse("reviews a model answer that no longer stands"))?;
        let label = review
            .answer(answer_id)
            .map(|answer| answer.label.clone())
            .unwrap_or_else(|| answer_id.to_string());
        match answer_id {
            MODEL_ANSWER_CONFIRM => {
                let comment = decision_comment(
                    review,
                    answer_id,
                    &label,
                    via,
                    note,
                    &format!(
                        "The model's answer `{}` to request {} stands, confirmed by {actor_label}.",
                        resolution.answer_id, original.id
                    ),
                );
                self.update_task_as_human(
                    &review.task_id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        ..Default::default()
                    },
                    actor_label.to_string(),
                )?;
                let confirmation = ModelAnswerConfirmation {
                    request_id: original.id.clone(),
                    answered_at: *answered_at,
                    confirmed_by: actor_label.to_string(),
                };
                let now = chrono::Utc::now();
                let confirmed = TaskHistoryEntry {
                    at: now,
                    by: actor_label.to_string(),
                    event: DECISION_MODEL_CONFIRMED_EVENT.to_string(),
                    note: serde_json::to_string(&confirmation).ok(),
                    from_status: None,
                    to_status: None,
                    operation_id: Some(original.id.clone()),
                    decision: None,
                };
                // A confirmed model answer enters the corpus outbox with the
                // confirming human as its answerer.
                let as_confirmed = DecisionResolution {
                    answered_by: actor_label.to_string(),
                    provenance: Some(via.to_string()),
                    note: note.map(str::to_string),
                    answered_at: now,
                    precedent: None,
                    model: None,
                    confirmation: None,
                    ..resolution
                };
                let corpus_append = self.decision_corpus_append(&original, &as_confirmed)?;
                self.stores().task_records().update(
                    &review.task_id,
                    TaskRecordUpdateParams {
                        actor: actor_label.to_string(),
                        append_history: std::iter::once(confirmed).chain(corpus_append).collect(),
                        ..Default::default()
                    },
                )?;
                self.record_model_answer_review_outcome(&original, false);
                Ok(())
            }
            MODEL_ANSWER_VETO => {
                let comment = decision_comment(
                    review,
                    answer_id,
                    &label,
                    via,
                    note,
                    &format!(
                        "Model answer vetoed by {actor_label}: request {} is open again for a human answer.",
                        original.id
                    ),
                );
                self.update_task_as_human(
                    &review.task_id,
                    TaskUpdateParams {
                        comment: Some(comment),
                        ..Default::default()
                    },
                    actor_label.to_string(),
                )?;
                self.reopen_automation_answer(&original, actor_label)?;
                self.record_model_answer_review_outcome(&original, true);
                Ok(())
            }
            other => Err(OrbitError::decision_refused(
                DecisionRefusalCode::AnswerNotAllowed,
                format!("`{other}` is not a model-answer review answer; nothing was applied"),
            )),
        }
    }

    /// Model answers awaiting human review, oldest first: each open
    /// `model_answer_review` request with the request it reviews.
    pub(crate) fn model_answered_digest(
        &self,
        open: &[DecisionRequest],
    ) -> Result<Vec<ModelAnsweredDigestEntry>, OrbitError> {
        let mut entries = Vec::new();
        for review in open
            .iter()
            .filter(|request| request.gate == DecisionGate::ModelAnswerReview)
        {
            let DecisionBinding::ModelAnswer { request_id, .. } = &review.binding else {
                continue;
            };
            let Some(request) = self
                .get_task_decision_requests(&review.task_id)?
                .into_iter()
                .find(|candidate| &candidate.id == request_id)
            else {
                continue;
            };
            entries.push(ModelAnsweredDigestEntry {
                review: review.clone(),
                request,
            });
        }
        Ok(entries)
    }
}

/// Test-support entry to the internal model apply step, so a regression
/// check drives it through the public runtime. No production surface calls
/// it.
#[cfg(feature = "test-support")]
impl OrbitRuntime {
    /// See [`OrbitRuntime::apply_model_decision_answer`].
    pub fn apply_model_decision_answer_for_test(
        &self,
        request_id: &str,
        answer_id: &str,
        citation: ModelAnswerCitation,
    ) -> Result<ModelAnswerApplied, OrbitError> {
        self.apply_model_decision_answer(request_id, answer_id, citation)
    }
}
