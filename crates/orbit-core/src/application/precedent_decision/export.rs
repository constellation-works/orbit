//! Join of sealed shadow predictions to human answers. Not a user surface.
use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::assistance::{AssistanceAnswer, AssistanceDisposition, AssistanceValue};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{DecisionRequestState, DecisionResolution, PRECEDENT_ACTOR};
use serde::Serialize;
use serde_json::Value;

use super::contract::request_category;
use super::ledger::{read_receipt, receipt_path};
use crate::OrbitRuntime;
use crate::application::assistance::AssistanceService;
use crate::application::assistance::AssistanceServicePort;

#[derive(Serialize)]
struct JoinDocument {
    format: &'static str,
    categories: BTreeMap<String, CategoryJoin>,
}

#[derive(Serialize)]
struct CategoryJoin {
    scored: Vec<ScoredRow>,
    counted: Counted,
}

#[derive(Default, Serialize)]
struct Counted {
    leaked: u64,
    curated_rule: u64,
    superseded: u64,
    expired: u64,
    unsealed: u64,
    automation: u64,
}

#[derive(Serialize)]
struct ScoredRow {
    request_id: String,
    category: String,
    raised_at: String,
    sealed_at: String,
    answered_at: String,
    answered_by: String,
    human_answer: String,
    choice: Option<String>,
    probability: Option<f64>,
    disposition: String,
    precedents_conflict: Option<bool>,
}

impl OrbitRuntime {
    /// Per-category join of sealed predictions and human answers.
    /// Predictions sealed at or after `answered_at`, curated-rule answers,
    /// and non-human actors are counted and omitted from `scored`.
    pub fn precedent_decision_join(&self) -> Result<Value, OrbitError> {
        let service = self.assistance_service()?;
        self.precedent_decision_join_with_service(&service)
    }

    /// [`Self::precedent_decision_join`] against the caller's assistance service.
    pub fn precedent_decision_join_with_service(
        &self,
        service: &AssistanceService,
    ) -> Result<Value, OrbitError> {
        let workspace = self.workspace_id()?;
        let mut categories: BTreeMap<String, CategoryJoin> = BTreeMap::new();
        for request in self.list_decision_requests(None, None, true)? {
            let Some(receipt) = read_receipt(&receipt_path(self, &request.id)?)? else {
                continue;
            };
            let category = if receipt.category.is_empty() {
                request_category(&request, &workspace)
            } else {
                receipt.category.clone()
            };
            let bucket = categories
                .entry(category.clone())
                .or_insert_with(|| CategoryJoin {
                    scored: Vec::new(),
                    counted: Counted::default(),
                });
            match request.state {
                DecisionRequestState::Superseded => bucket.counted.superseded += 1,
                DecisionRequestState::Expired => bucket.counted.expired += 1,
                DecisionRequestState::Open => {
                    if receipt.sealed_at.is_none() {
                        bucket.counted.unsealed += 1;
                    }
                }
                DecisionRequestState::Answered => {
                    classify_answered(&request, &receipt, service, bucket, &category)?;
                }
            }
        }
        serde_json::to_value(JoinDocument {
            format: "precedent-decision-join/1",
            categories,
        })
        .map_err(super::ledger::ledger_json_error)
    }
}

fn classify_answered(
    request: &orbit_types::task::DecisionRequest,
    receipt: &super::ledger::PrecedentDecisionReceipt,
    service: &AssistanceService,
    bucket: &mut CategoryJoin,
    category: &str,
) -> Result<(), OrbitError> {
    let Some(resolution) = request.resolution.as_ref() else {
        bucket.counted.unsealed += 1;
        return Ok(());
    };
    if is_curated(resolution) {
        bucket.counted.curated_rule += 1;
        return Ok(());
    }
    if is_non_human(resolution) {
        bucket.counted.automation += 1;
        return Ok(());
    }
    let Some(sealed_at) = receipt.sealed_at else {
        bucket.counted.unsealed += 1;
        return Ok(());
    };
    if sealed_at >= resolution.answered_at {
        bucket.counted.leaked += 1;
        return Ok(());
    }
    let prediction = prediction(service, receipt)?;
    bucket.scored.push(ScoredRow {
        request_id: request.id.clone(),
        category: category.to_string(),
        raised_at: request.raised_at.to_rfc3339(),
        sealed_at: sealed_at.to_rfc3339(),
        answered_at: resolution.answered_at.to_rfc3339(),
        answered_by: resolution.answered_by.clone(),
        human_answer: resolution.answer_id.clone(),
        choice: prediction.choice,
        probability: prediction.probability,
        disposition: prediction.disposition,
        precedents_conflict: prediction.precedents_conflict,
    });
    Ok(())
}

fn is_curated(resolution: &DecisionResolution) -> bool {
    resolution.answered_by.trim() == PRECEDENT_ACTOR || resolution.precedent.is_some()
}

fn is_non_human(resolution: &DecisionResolution) -> bool {
    let actor = resolution.answered_by.trim();
    actor == "model" || is_automation_actor(actor)
}

struct Prediction {
    choice: Option<String>,
    probability: Option<f64>,
    disposition: String,
    precedents_conflict: Option<bool>,
}

fn prediction(
    service: &AssistanceService,
    receipt: &super::ledger::PrecedentDecisionReceipt,
) -> Result<Prediction, OrbitError> {
    let Some(work_id) = receipt.work_id.as_deref() else {
        return Ok(Prediction {
            choice: None,
            probability: None,
            disposition: "unavailable".into(),
            precedents_conflict: None,
        });
    };
    let Some(record) = service.assistance_assessment(work_id, &receipt.source_digest)? else {
        return Ok(Prediction {
            choice: None,
            probability: None,
            disposition: "unavailable".into(),
            precedents_conflict: None,
        });
    };
    let Some(result) = record.result.as_ref() else {
        return Ok(Prediction {
            choice: None,
            probability: None,
            disposition: "unavailable".into(),
            precedents_conflict: None,
        });
    };
    let choice_answer = result.answers.get("decision_choice");
    Ok(Prediction {
        choice: choice_answer.and_then(choice_value),
        probability: choice_answer.and_then(|answer| answer.probability),
        disposition: disposition_name(result.disposition).into(),
        precedents_conflict: result
            .answers
            .get("precedents_conflict")
            .and_then(|answer| match answer.value {
                AssistanceValue::Predicate(value) => Some(value),
                AssistanceValue::Choice(_) => None,
            }),
    })
}

fn choice_value(answer: &AssistanceAnswer) -> Option<String> {
    match &answer.value {
        AssistanceValue::Choice(value) => Some(value.clone()),
        AssistanceValue::Predicate(_) => None,
    }
}

fn disposition_name(disposition: AssistanceDisposition) -> &'static str {
    match disposition {
        AssistanceDisposition::Suggested => "suggested",
        AssistanceDisposition::Uncertain => "uncertain",
        AssistanceDisposition::Refused => "refused",
        AssistanceDisposition::Unavailable => "unavailable",
        AssistanceDisposition::Invalid => "invalid",
        AssistanceDisposition::Stale => "stale",
    }
}
