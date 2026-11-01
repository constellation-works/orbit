//! Historical retrieval never uses the current task text or folded request
//! state: either could reveal changes made after the query's cutoff.

use std::collections::{BTreeMap, BTreeSet};

use orbit_types::task::{
    DECISION_ANSWERED_EVENT, DECISION_REOPENED_EVENT, DecisionGate, DecisionRequest,
    DecisionResolution, TaskHistoryEntry,
};
use serde_json::Value;

use super::model::*;
use super::validation;

/// Read the immutable request/answer events in one task history. This public
/// boundary also lets consumers replay a historical packet without a runtime.
/// The runtime applies response validation and ranking to the combined scan.
pub fn decision_precedents_from_history(
    query: &PrecedentProviderRequest,
    history: &[TaskHistoryEntry],
) -> Vec<DecisionPrecedent> {
    let requests: BTreeMap<_, _> = history
        .iter()
        .filter(|entry| entry.at < query.as_of)
        .filter_map(|entry| {
            let request = DecisionRequest::from_entry(entry)?;
            if request.raised_at >= query.as_of {
                return None;
            }
            // Prefer the stored category when the request contract has one.
            let category = entry
                .note
                .as_deref()
                .and_then(|note| serde_json::from_str::<Value>(note).ok())
                .and_then(|note| {
                    note.get("category")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| decision_precedent_category(&query.workspace_id, &request));
            Some((request.id.clone(), (request, category)))
        })
        .collect();
    let mut events: Vec<_> = history
        .iter()
        .filter(|entry| entry.at < query.as_of)
        .collect();
    // Histories are normally ordered. Stable sorting also makes replay of an
    // imported history chronological while preserving equal-time event order.
    events.sort_by_key(|entry| entry.at);
    let mut precedents: Vec<DecisionPrecedent> = Vec::new();
    for (index, entry) in events.iter().enumerate() {
        if entry.event != DECISION_ANSWERED_EVENT {
            continue;
        }
        let Some(resolution) = entry
            .note
            .as_deref()
            .and_then(|note| serde_json::from_str::<DecisionResolution>(note).ok())
        else {
            continue;
        };
        let Some((request, category)) = requests.get(&resolution.request_id) else {
            continue;
        };
        if resolution.answered_at >= query.as_of || resolution.answered_at < request.raised_at {
            continue;
        }
        let (decided_by, decided_by_kind, decided_at) =
            if validation::precedent_human_actor(&entry.by)
                && validation::precedent_human_actor(&resolution.answered_by)
                && entry.by == resolution.answered_by
                && resolution.precedent.is_none()
            {
                (
                    resolution.answered_by.clone(),
                    PrecedentDeciderKind::Human,
                    resolution.answered_at,
                )
            } else if entry.by == "model" && resolution.answered_by == "model" {
                let confirmation = events
                    .iter()
                    .skip(index + 1)
                    .take_while(|event| !decision_precedent_reopened(event, &request.id))
                    .find_map(|event| {
                        if event.event != PRECEDENT_HUMAN_CONFIRMED_EVENT
                            || !validation::precedent_human_actor(&event.by)
                        {
                            return None;
                        }
                        let confirmation: PrecedentHumanConfirmation =
                            serde_json::from_str(event.note.as_deref()?).ok()?;
                        (confirmation.request_id == request.id
                            && confirmation.answered_at == resolution.answered_at
                            && confirmation.confirmed_by == event.by
                            && event.at >= resolution.answered_at)
                            .then_some((confirmation.confirmed_by, event.at))
                    });
                let Some((human, at)) = confirmation else {
                    continue;
                };
                (human, PrecedentDeciderKind::HumanConfirmed, at)
            } else {
                continue;
            };
        let reversed = events
            .iter()
            .skip(index + 1)
            .any(|event| decision_precedent_reopened(event, &request.id));
        let reference = format!(
            "decision:{}:{}",
            request.id,
            resolution.answered_at.to_rfc3339()
        );
        // Content-address the immutable source events, including the request's
        // original category and options. Never dereference this citation.
        let digest = orbit_types::assistance::assistance_digest(
            &serde_json::to_vec(&(request, category, &resolution, &decided_by, decided_at))
                .unwrap_or_default(),
        );
        precedents.push(DecisionPrecedent {
            id: reference.clone(),
            question: request
                .summary
                .clone()
                .unwrap_or_else(|| request.gate.as_str().to_string()),
            options: request
                .answers
                .iter()
                .map(|answer| PrecedentOption {
                    id: answer.id.clone(),
                    label: answer.label.clone(),
                    consequence: Some(answer.consequence.clone()),
                })
                .collect(),
            chosen_option: resolution.answer_id,
            context: resolution.note.unwrap_or_default(),
            scope: PrecedentScope {
                workspace_id: query.workspace_id.clone(),
                category: category.clone(),
                topic: request.topic.clone(),
            },
            decided_at,
            decided_by,
            decided_by_kind,
            reversed,
            citation: PrecedentProviderCitation {
                source: "orbit-decision-history".into(),
                reference,
                digest,
            },
        });
    }
    precedents
}

fn decision_precedent_reopened(event: &TaskHistoryEntry, request_id: &str) -> bool {
    event.event == DECISION_REOPENED_EVENT
        && event
            .note
            .as_deref()
            .and_then(|note| serde_json::from_str::<Value>(note).ok())
            .is_some_and(|note| note.get("request_id").and_then(Value::as_str) == Some(request_id))
}

/// Fallback for requests written before the stored category field existed.
pub fn decision_precedent_category(workspace: &str, request: &DecisionRequest) -> String {
    if request.gate == DecisionGate::AgentRequest {
        let file = request
            .topic
            .as_deref()
            .map(|topic| topic.split('#').next().unwrap_or(topic))
            .unwrap_or("untopiced");
        format!("{workspace}/agent_request/{file}")
    } else {
        format!("{workspace}/{}", request.gate.as_str())
    }
}

pub(super) fn rank_history_precedents(
    query: &PrecedentProviderRequest,
    records: &mut Vec<DecisionPrecedent>,
) {
    let words = precedent_question_words(&query.request.question);
    let rank = |record: &DecisionPrecedent| {
        (
            record.scope.category == query.request.category,
            query.request.topic.is_some() && record.scope.topic == query.request.topic,
            precedent_question_words(&record.question)
                .intersection(&words)
                .count(),
            record.decided_at,
        )
    };
    records.sort_by(|a, b| rank(b).cmp(&rank(a)).then_with(|| a.id.cmp(&b.id)));
    records.truncate(query.k);
}

fn precedent_question_words(question: &str) -> BTreeSet<String> {
    question
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}
