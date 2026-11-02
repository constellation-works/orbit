//! Self-answer in enabled categories: holdback, the notification wait, the
//! answer checks, the model-actor apply, and the rolling demotion window.
//!
//! Only a request whose category the operator enabled under the live grant
//! is ever considered. Its tier must be `routine` or `reversible`; a keyed
//! holdback share goes to the human anyway; the assessment must have settled
//! within `assist_wait_seconds` of the raise as a `suggested` choice at or
//! above the enabled threshold, with a supporting precedent and no conflict
//! (the model's flag or a deterministic contradiction in the packet). Only
//! then does core apply the answer as the `model` actor, through the same
//! apply step every model answer uses. Every other request is routed to the
//! human exactly as before, and holdback and excluded-tier predictions are
//! scored against the human's answer for the rolling window.
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::assistance::AssistanceRecord;
use orbit_types::assistance::{
    AssistanceAnswer, AssistanceDisposition, AssistanceReason, AssistanceValue,
};
use orbit_types::identity::is_automation_actor;
use orbit_types::task::{
    DecisionRequest, DecisionRequestState, DecisionRiskTier, MODEL_ANSWER_ACTOR,
    ModelAnswerCitation, ModelAnswerEnablement, ModelCitedPrecedent, PRECEDENT_ACTOR,
};
use serde_json::Value;

use super::contract::{ABSTAIN, PRECEDENT_DECISION_USE_CASE, QUESTION_SET, request_category};
use super::enablement::{
    PRECEDENT_HALF_LIFE_DAYS, PrecedentAssistEnablement, PrecedentAssistEnd,
    PrecedentAssistOutcome, PrecedentAssistOutcomeSource, PrecedentAssistState,
    PrecedentAssistStore, window_demotion,
};
use super::ledger::{
    PrecedentAssistRouting, PrecedentDecisionReceipt, read_receipt, receipt_dir, receipt_path,
    with_receipt, write_receipt,
};
use crate::OrbitRuntime;
use crate::application::assistance::{AssistancePolicyIdentity, AssistanceService};

const APPLY_BOUND: usize = 32;

fn self_answerable(tier: DecisionRiskTier) -> bool {
    matches!(
        tier,
        DecisionRiskTier::Routine | DecisionRiskTier::Reversible
    )
}

fn wait_deadline(record: &PrecedentAssistEnablement, raised_at: DateTime<Utc>) -> DateTime<Utc> {
    raised_at + Duration::seconds(i64::try_from(record.assist_wait_seconds).unwrap_or(i64::MAX))
}

/// A covered prediction: a complete, non-abstaining choice at or above the
/// threshold with at least one supporting, unreversed precedent.
struct Covered {
    choice: String,
    probability: f64,
    supporting: Vec<Value>,
}

fn predicate_probability(answer: Option<&AssistanceAnswer>) -> f64 {
    answer.map_or(0.0, |answer| {
        answer.probability.unwrap_or(match answer.value {
            AssistanceValue::Predicate(true) => 1.0,
            _ => 0.0,
        })
    })
}

fn precedent_rows(record: &AssistanceRecord) -> Vec<Value> {
    record
        .work
        .request
        .evidence_packet
        .fields
        .get("precedents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn covered(
    enablement: &PrecedentAssistEnablement,
    record: &AssistanceRecord,
) -> Result<Covered, &'static str> {
    if !record.settled || !record.current {
        return Err("stale");
    }
    let result = record.result.as_ref().ok_or("unavailable")?;
    // The granted template sets no per-question `min_confidence`, so the
    // service reports a complete, valid answer set as `uncertain`. The host
    // derives this consumer's `suggested` itself: a complete result at or
    // above the enabled threshold, grounded in a supporting precedent and
    // (in `qualify`) free of conflict. A refused, invalid, stale or
    // unavailable result never qualifies.
    let complete = result.disposition == AssistanceDisposition::Suggested
        || (result.disposition == AssistanceDisposition::Uncertain
            && result.reason == AssistanceReason::Complete);
    if !complete {
        return Err("not_suggested");
    }
    let answer = result
        .answers
        .get("decision_choice")
        .ok_or("not_suggested")?;
    let AssistanceValue::Choice(choice) = &answer.value else {
        return Err("not_suggested");
    };
    if choice == ABSTAIN {
        return Err("abstained");
    }
    let probability = answer.probability.ok_or("not_suggested")?;
    // A NaN on either side is incomparable and must never clear the bar.
    if !matches!(
        probability.partial_cmp(&enablement.threshold),
        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
    ) {
        return Err("below_threshold");
    }
    let rows = precedent_rows(record);
    let supporting: Vec<Value> = rows
        .iter()
        .enumerate()
        .filter(|(index, row)| {
            row["reversed"].as_bool() != Some(true)
                && predicate_probability(
                    result
                        .answers
                        .get(&format!("precedent_supports/{}", index + 1)),
                ) >= 0.5
        })
        .map(|(_, row)| row.clone())
        .collect();
    if supporting.is_empty() {
        return Err("no_supporting_precedent");
    }
    Ok(Covered {
        choice: choice.clone(),
        probability,
        supporting,
    })
}

fn chosen_label(row: &Value) -> Option<String> {
    let chosen = row["chosen_option"].as_str()?;
    row["options"]
        .as_array()?
        .iter()
        .find(|option| option["id"].as_str() == Some(chosen))
        .and_then(|option| option["label"].as_str())
        .map(|label| label.trim().to_lowercase())
}

/// Deterministic contradiction in the packet, whatever the model said: a
/// reversed precedent in the same category and topic within the half-life,
/// or two recent human answers to the same topic choosing different options.
fn deterministic_contradiction(request: &DecisionRequest, category: &str, rows: &[Value]) -> bool {
    let recent = |row: &Value| {
        row["age_days"]
            .as_i64()
            .is_some_and(|age| age <= PRECEDENT_HALF_LIFE_DAYS)
    };
    let topic = request.topic.as_deref();
    let same_topic = |row: &Value| topic.is_some() && row["topic"].as_str() == topic;
    if rows.iter().any(|row| {
        row["reversed"].as_bool() == Some(true)
            && row["category"].as_str() == Some(category)
            && same_topic(row)
            && recent(row)
    }) {
        return true;
    }
    let mut labels = std::collections::BTreeSet::new();
    for row in rows {
        if row["reversed"].as_bool() != Some(true)
            && same_topic(row)
            && recent(row)
            && let Some(label) = chosen_label(row)
        {
            labels.insert(label);
        }
    }
    labels.len() > 1
}

fn cited(row: &Value) -> Option<ModelCitedPrecedent> {
    let citation = &row["citation"];
    Some(ModelCitedPrecedent {
        provider: citation["source"].as_str()?.to_string(),
        precedent_id: row["id"].as_str()?.to_string(),
        decided_at: row["decided_at"].as_str()?.parse().ok()?,
        source: citation["source"].as_str()?.to_string(),
        reference: citation["ref"].as_str()?.to_string(),
        digest: citation["digest"].as_str()?.to_string(),
    })
}

/// Every self-answer check past enablement, tier, holdback and wait. Returns
/// the answer and the citation the apply step records.
fn qualify(
    enablement: &PrecedentAssistEnablement,
    request: &DecisionRequest,
    category: &str,
    record: &AssistanceRecord,
) -> Result<(String, ModelAnswerCitation), &'static str> {
    if record.work.policy_digest != enablement.policy_digest
        || record.work.model_revision != enablement.model_revision
    {
        return Err("stale");
    }
    if record.work.request.evidence_packet.truncated {
        return Err("truncated");
    }
    let covered = covered(enablement, record)?;
    if request.answer(&covered.choice).is_none() {
        return Err("stale");
    }
    let result = record.result.as_ref().ok_or("unavailable")?;
    let conflict = predicate_probability(result.answers.get("precedents_conflict"));
    if conflict >= 0.5 {
        return Err("conflict");
    }
    if deterministic_contradiction(request, category, &precedent_rows(record)) {
        return Err("contradiction");
    }
    let cited_precedents: Vec<ModelCitedPrecedent> =
        covered.supporting.iter().filter_map(cited).collect();
    if cited_precedents.len() != covered.supporting.len() {
        return Err("invalid_citation");
    }
    let citation = ModelAnswerCitation {
        assessment_id: record.work.id.clone(),
        logical_id: record.work.slot.clone(),
        category: category.to_string(),
        policy_version: enablement.policy_version.clone(),
        policy_digest: record.work.policy_digest.clone(),
        question_set: QUESTION_SET.into(),
        contract_version: record.work.request.contract.format_version.clone(),
        adapter: record.work.adapter.clone(),
        requested_model: record.work.requested_model.clone(),
        returned_model: result
            .returned_model
            .clone()
            .unwrap_or_else(|| "unknown".into()),
        model_revision: record.work.model_revision.clone(),
        probability: covered.probability,
        threshold: enablement.threshold,
        conflict_probability: conflict.clamp(0.0, 1.0),
        cited_precedents,
        enablement: ModelAnswerEnablement {
            operator: enablement.operator.clone(),
            readout: enablement.readout.clone(),
        },
        previous_status: None,
    };
    Ok((covered.choice, citation))
}

fn human_answer(request: &DecisionRequest) -> Option<(&str, DateTime<Utc>)> {
    let resolution = request.resolution.as_ref()?;
    let actor = resolution.answered_by.trim();
    if request.state != DecisionRequestState::Answered
        || actor == MODEL_ANSWER_ACTOR
        || actor == PRECEDENT_ACTOR
        || resolution.precedent.is_some()
        || resolution.model.is_some()
        || is_automation_actor(actor)
    {
        return None;
    }
    Some((resolution.answer_id.as_str(), resolution.answered_at))
}

impl OrbitRuntime {
    /// Whether the `DecisionRequested` notification for `request` waits for
    /// its assessment: only an open, self-answerable request in a category
    /// enabled under the live grant, outside the holdback, before
    /// `assist_wait_seconds` elapse and before the request was routed. Any
    /// error reads as "notify now".
    pub(crate) fn precedent_decision_notify_wait(&self, request: &DecisionRequest) -> bool {
        if request.state != DecisionRequestState::Open || !self_answerable(request.risk_tier) {
            return false;
        }
        let Ok(workspace) = self.workspace_id() else {
            return false;
        };
        let Ok(store) = self.precedent_assist_store() else {
            return false;
        };
        let category = request_category(request, &workspace);
        let Some(record) = store
            .categories
            .get(&category)
            .filter(|record| record.state == PrecedentAssistState::Enabled)
        else {
            return false;
        };
        if Utc::now() >= wait_deadline(record, request.raised_at)
            || store.held_back(&category, &request.id, record.holdback_share)
        {
            return false;
        }
        let routed = receipt_path(self, &request.id)
            .and_then(|path| read_receipt(&path))
            .map(|receipt| receipt.is_some_and(|receipt| receipt.assist.is_some()));
        if !matches!(routed, Ok(false)) {
            return false;
        }
        let live = self.precedent_decision_service().and_then(|service| {
            service.assistance_shadow_policy_identity(&workspace, PRECEDENT_DECISION_USE_CASE)
        });
        matches!(live, Ok(live) if record.answers_under(live.as_ref()))
    }

    /// The owner tick's enabled-category step: suspend enablements whose
    /// grant moved, route each sealed request, apply qualifying model
    /// answers, and score holdback and excluded-tier predictions. Returns at
    /// once when this workspace has no enabled category.
    pub(super) fn run_precedent_assist(
        &self,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        let workspace = self.workspace_id()?;
        let Ok(store) = self.precedent_assist_store() else {
            return Ok(());
        };
        if !store.categories.values().any(|record| {
            record.state == PrecedentAssistState::Enabled && record.workspace() == workspace
        }) {
            return Ok(());
        }
        let live =
            service.assistance_shadow_policy_identity(&workspace, PRECEDENT_DECISION_USE_CASE)?;
        self.suspend_moved_precedent_assist(&workspace, live.as_ref())?;
        let workspace_digest = orbit_types::assistance::assistance_digest(workspace.as_bytes());
        let dir = receipt_dir(self, &workspace_digest)?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut applied = 0;
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            // Re-read per receipt so a disable or demotion is seen at once.
            let Ok(store) = self.precedent_assist_store() else {
                return Ok(());
            };
            let Some(receipt) = read_receipt(&path)? else {
                continue;
            };
            let Some(record) = store
                .categories
                .get(&receipt.category)
                .filter(|record| record.answers_under(live.as_ref()))
            else {
                continue;
            };
            match receipt.assist.as_ref() {
                None if applied < APPLY_BOUND => {
                    if self.route_precedent_request(&path, &store, record, service)? {
                        applied += 1;
                    }
                }
                Some(routing)
                    if !routing.scored
                        && matches!(routing.route.as_str(), "holdback" | "excluded_tier") =>
                {
                    self.score_precedent_prediction(&path, record, service)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Route one unrouted receipt. Returns whether a model answer was applied.
    fn route_precedent_request(
        &self,
        path: &std::path::Path,
        store: &PrecedentAssistStore,
        record: &PrecedentAssistEnablement,
        service: &AssistanceService,
    ) -> Result<bool, OrbitError> {
        let mut applied = false;
        with_receipt(path, || {
            let Some(mut receipt) = read_receipt(path)? else {
                return Ok(());
            };
            if receipt.assist.is_some() {
                return Ok(());
            }
            let Some(request) = self.get_decision_request(&receipt.request_id)? else {
                return Ok(());
            };
            let now = Utc::now();
            let deadline = wait_deadline(record, request.raised_at);
            let route = if !self_answerable(request.risk_tier) {
                "excluded_tier".to_string()
            } else if store.held_back(&receipt.category, &request.id, record.holdback_share) {
                "holdback".to_string()
            } else if request.state != DecisionRequestState::Open {
                "closed".to_string()
            } else {
                match receipt.sealed_at {
                    None if now < deadline => return Ok(()),
                    None => "declined:timeout".to_string(),
                    Some(sealed) if sealed > deadline => "declined:timeout".to_string(),
                    Some(_) => {
                        self.try_precedent_self_answer(&receipt, &request, service, &mut applied)?
                    }
                }
            };
            receipt.assist = Some(PrecedentAssistRouting {
                route,
                at: now,
                scored: false,
            });
            write_receipt(path, &receipt)
        })?;
        Ok(applied)
    }

    fn try_precedent_self_answer(
        &self,
        receipt: &PrecedentDecisionReceipt,
        request: &DecisionRequest,
        service: &AssistanceService,
        applied: &mut bool,
    ) -> Result<String, OrbitError> {
        let Some(work_id) = receipt.work_id.as_deref() else {
            return Ok("declined:unavailable".into());
        };
        let Some(record) = service.assistance_assessment(work_id, &receipt.source_digest)? else {
            return Ok("declined:unavailable".into());
        };
        // Fresh enablement read immediately before applying: a disable,
        // demotion or grant change since the routing pass started wins.
        let workspace = self.workspace_id()?;
        let live =
            service.assistance_shadow_policy_identity(&workspace, PRECEDENT_DECISION_USE_CASE)?;
        let Some(enablement) = self
            .precedent_assist_store()?
            .categories
            .get(&receipt.category)
            .filter(|enablement| enablement.answers_under(live.as_ref()))
            .cloned()
        else {
            return Ok("declined:not_enabled".into());
        };
        let (choice, citation) = match qualify(&enablement, request, &receipt.category, &record) {
            Ok(qualified) => qualified,
            Err(reason) => return Ok(format!("declined:{reason}")),
        };
        match self.apply_model_decision_answer(&request.id, &choice, citation) {
            Ok(_) => {
                *applied = true;
                Ok("self_answered".into())
            }
            Err(error) => {
                tracing::debug!(%error, "precedent self-answer refused by the apply step");
                Ok("declined:apply_refused".into())
            }
        }
    }

    fn score_precedent_prediction(
        &self,
        path: &std::path::Path,
        record: &PrecedentAssistEnablement,
        service: &AssistanceService,
    ) -> Result<(), OrbitError> {
        let mut outcome = None;
        with_receipt(path, || {
            let Some(mut receipt) = read_receipt(path)? else {
                return Ok(());
            };
            let Some(routing) = receipt.assist.as_mut() else {
                return Ok(());
            };
            if routing.scored {
                return Ok(());
            }
            let source = if routing.route == "holdback" {
                PrecedentAssistOutcomeSource::Holdback
            } else {
                PrecedentAssistOutcomeSource::ExcludedTier
            };
            let request = self.get_decision_request(&receipt.request_id)?;
            if request
                .as_ref()
                .is_some_and(|request| request.state == DecisionRequestState::Open)
            {
                return Ok(());
            }
            let human = request.as_ref().and_then(human_answer);
            if let (Some((answer, answered_at)), Some(sealed_at), Some(work_id)) =
                (human, receipt.sealed_at, receipt.work_id.as_deref())
                && sealed_at < answered_at
                && let Some(assessment) =
                    service.assistance_assessment(work_id, &receipt.source_digest)?
                && let Ok(covered) = covered(record, &assessment)
            {
                outcome = Some(PrecedentAssistOutcome {
                    request_id: receipt.request_id.clone(),
                    at: answered_at,
                    source,
                    agreed: covered.choice == answer,
                });
            }
            routing.scored = true;
            write_receipt(path, &receipt)
        })?;
        if let Some(outcome) = outcome {
            self.record_precedent_assist_outcome(&record.category, outcome)?;
        }
        Ok(())
    }

    /// A human confirmed or vetoed the model answer to `original`. Best
    /// effort: the review's own write never fails on the window.
    pub(crate) fn note_precedent_assist_review(&self, original: &DecisionRequest, vetoed: bool) {
        let Some(category) = original
            .resolution
            .as_ref()
            .and_then(|resolution| resolution.model.as_ref())
            .map(|model| model.category.clone())
        else {
            return;
        };
        let outcome = PrecedentAssistOutcome {
            request_id: original.id.clone(),
            at: Utc::now(),
            source: if vetoed {
                PrecedentAssistOutcomeSource::Vetoed
            } else {
                PrecedentAssistOutcomeSource::Confirmed
            },
            agreed: !vetoed,
        };
        if let Err(error) = self.record_precedent_assist_outcome(&category, outcome) {
            tracing::debug!(%error, "precedent assist window update deferred");
        }
    }

    /// Append one outcome to an enabled category's window and demote the
    /// category when the window now fails its bar.
    fn record_precedent_assist_outcome(
        &self,
        category: &str,
        outcome: PrecedentAssistOutcome,
    ) -> Result<(), OrbitError> {
        let _lock = self.lock_precedent_assist_store()?;
        let mut store = self.precedent_assist_store()?;
        let Some(record) = store
            .categories
            .get_mut(category)
            .filter(|record| record.state == PrecedentAssistState::Enabled)
        else {
            return Ok(());
        };
        if record
            .window
            .iter()
            .any(|seen| seen.request_id == outcome.request_id && seen.source == outcome.source)
        {
            return Ok(());
        }
        record.window.push(outcome);
        let keep = usize::try_from(record.demotion_window)
            .unwrap_or(usize::MAX)
            .max(1);
        if record.window.len() > keep {
            let excess = record.window.len() - keep;
            record.window.drain(..excess);
        }
        if let Some(demotion) = window_demotion(record) {
            tracing::info!(
                target: "orbit.core.precedent_decision",
                category = %record.category,
                trigger = %demotion.trigger,
                "precedent-decision category demoted to shadow"
            );
            record.state = PrecedentAssistState::Demoted;
            record.ended = Some(PrecedentAssistEnd {
                at: Utc::now(),
                by: "system".into(),
                reason: "demoted".into(),
                demotion: Some(demotion),
            });
        }
        self.write_precedent_assist_store(&mut store)
    }

    /// Persist the suspension of every enabled category in `workspace`
    /// whose live grant digest or model revision moved.
    fn suspend_moved_precedent_assist(
        &self,
        workspace: &str,
        live: Option<&AssistancePolicyIdentity>,
    ) -> Result<(), OrbitError> {
        let store = self.precedent_assist_store()?;
        if !store.categories.values().any(|record| {
            record.state == PrecedentAssistState::Enabled
                && record.workspace() == workspace
                && record.suspension(live).is_some()
        }) {
            return Ok(());
        }
        let _lock = self.lock_precedent_assist_store()?;
        let mut store = self.precedent_assist_store()?;
        let now = Utc::now();
        for record in store.categories.values_mut() {
            if record.state != PrecedentAssistState::Enabled || record.workspace() != workspace {
                continue;
            }
            if let Some(reason) = record.suspension(live) {
                record.state = PrecedentAssistState::Suspended;
                record.ended = Some(PrecedentAssistEnd {
                    at: now,
                    by: "system".into(),
                    reason: reason.into(),
                    demotion: None,
                });
            }
        }
        self.write_precedent_assist_store(&mut store)
    }
}
