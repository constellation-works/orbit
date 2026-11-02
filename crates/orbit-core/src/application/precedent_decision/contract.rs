//! Granted `precedent-decision/1` template and the packet fields it binds.
use std::collections::BTreeMap;

use orbit_types::assistance::{
    self, AssistanceContract, AssistancePacketBoundChoice, AssistanceQuestion,
};
use orbit_types::task::{DecisionGate, DecisionRequest};

pub const PRECEDENT_DECISION_USE_CASE: &str = "precedent_decision";
pub(super) const QUESTION_SET: &str = "precedent-decision/1";
pub(super) const RUBRIC_VERSION: &str = "precedent-decision-rubric/1";
pub(super) const BUILDER_VERSION: &str = "precedent-decision-evidence/1";
pub(super) const ABSTAIN: &str = "insufficient_precedent";
pub(super) const PRECEDENT_K: usize = 8;
pub(super) const CONTEXT_BYTES: usize = 2_000;

/// Host-granted question set. Option ids are copied from packet field
/// `option_ids` at admission; the digest covers the binding, not the ids.
pub fn precedent_decision_question_template() -> AssistanceContract {
    AssistanceContract {
        format_version: assistance::ASSISTANCE_FORMAT.into(),
        question_set_version: QUESTION_SET.into(),
        rubric_version: RUBRIC_VERSION.into(),
        evidence_builder_version: BUILDER_VERSION.into(),
        result_schema_version: assistance::ASSISTANCE_RESULT_SCHEMA.into(),
        questions: BTreeMap::from([
            (
                "decision_choice".into(),
                AssistanceQuestion::Choice {
                    rubric: "Choose the option the retained precedents support. Abstain when they do not support one option.".into(),
                    allowed: vec![ABSTAIN.into()],
                    abstain_values: vec![ABSTAIN.into()],
                    min_confidence: None,
                    criterion_refs: Vec::new(),
                    packet_bound: Some(AssistancePacketBoundChoice {
                        field: "option_ids".into(),
                        max_ids: assistance::ASSISTANCE_PACKET_BOUND_MAX_IDS,
                        max_id_bytes: assistance::ASSISTANCE_PACKET_BOUND_MAX_ID_BYTES,
                    }),
                },
            ),
            (
                "precedent_supports".into(),
                AssistanceQuestion::Predicate {
                    rubric: "True only when this precedent supports the chosen option and is not reversed.".into(),
                    min_confidence: None,
                    criterion_refs: Vec::new(),
                },
            ),
            (
                "precedents_conflict".into(),
                AssistanceQuestion::Predicate {
                    rubric: "True when the retained precedents disagree about which option to choose.".into(),
                    min_confidence: None,
                    criterion_refs: Vec::new(),
                },
            ),
        ]),
    }
}

pub(super) fn options_admissible(request: &DecisionRequest) -> bool {
    let mut seen = BTreeMap::<&str, ()>::new();
    if request.answers.is_empty()
        || request.answers.len() > usize::from(assistance::ASSISTANCE_PACKET_BOUND_MAX_IDS)
    {
        return false;
    }
    request.answers.iter().all(|answer| {
        assistance::assistance_identifier(&answer.id)
            && answer.id != ABSTAIN
            && seen.insert(answer.id.as_str(), ()).is_none()
    })
}

pub(super) fn request_category(request: &DecisionRequest, workspace: &str) -> String {
    if let Some(category) = request
        .category
        .as_deref()
        .map(str::trim)
        .filter(|category| !category.is_empty())
    {
        return category.to_string();
    }
    let kind = if request.gate == DecisionGate::AgentRequest {
        let file = request
            .topic
            .as_deref()
            .map(|topic| topic.split('/').next().unwrap_or(topic))
            .filter(|file| !file.is_empty())
            .unwrap_or("untopiced");
        format!("agent_request/{file}")
    } else {
        request.gate.as_str().to_string()
    };
    format!("{workspace}/{kind}")
}

pub(super) fn question_text(request: &DecisionRequest) -> String {
    if let Some(summary) = request
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|summary| !summary.is_empty())
    {
        return summary.to_string();
    }
    match request.gate {
        DecisionGate::BlockTriage => "How should this blocked task proceed?",
        DecisionGate::PilotAdmission => "How should this pilot-admission hold be resolved?",
        DecisionGate::ProposedApproval => "Should this proposed task be approved?",
        DecisionGate::ReviewApproval => "Should this task in review be approved?",
        DecisionGate::AgentRequest => "Which option answers this request?",
        DecisionGate::ModelAnswerReview => "Should the model's answer stand?",
        DecisionGate::ResidentAuthority => "Should the resident's authority be widened?",
        DecisionGate::ScriptGrant => "Should this script be granted at this digest?",
    }
    .to_string()
}

pub(super) fn bound_text(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

pub(super) fn risk_tier_name(tier: orbit_types::task::DecisionRiskTier) -> &'static str {
    match tier {
        orbit_types::task::DecisionRiskTier::Routine => "routine",
        orbit_types::task::DecisionRiskTier::Reversible => "reversible",
        orbit_types::task::DecisionRiskTier::Irreversible => "irreversible",
        orbit_types::task::DecisionRiskTier::Authority => "authority",
        orbit_types::task::DecisionRiskTier::Spend => "spend",
        orbit_types::task::DecisionRiskTier::Unknown => "unknown",
    }
}
