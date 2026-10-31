use super::{
    AssistanceContract, AssistanceCriterionRef, AssistanceQuestion, assistance_probability,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssistanceValue {
    Predicate(bool),
    Choice(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceProbability {
    pub value: String,
    pub probability: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceAnswer {
    pub value: AssistanceValue,
    /// Predicate's probability of true, or the choice's confidence.
    pub probability: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probabilities: Vec<AssistanceProbability>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub criterion_refs: Vec<AssistanceCriterionRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistanceDisposition {
    Suggested,
    Uncertain,
    Refused,
    Unavailable,
    Invalid,
    Stale,
}

/// Fixed codes only: provider errors, bodies and supplied secrets never enter
/// the durable diagnostic surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistanceReason {
    Complete,
    InvalidRequest,
    MissingAnswers,
    Malformed,
    Refused,
    Unavailable,
    TimeoutUnknown,
    ClaimExpiredUnknown,
    /// The claim deadline passed before any outbound request was built. Nothing
    /// was sent, so the claim releases its reservation instead of counting unknown.
    DeadlineBeforeSend,
    QueueExpired,
    QueueFull,
    BudgetExhausted,
    PricingUnknown,
    CredentialUnavailable,
    ConfigurationChanged,
    Superseded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistanceResult {
    pub disposition: AssistanceDisposition,
    pub reason: AssistanceReason,
    pub answers: BTreeMap<String, AssistanceAnswer>,
    pub returned_model: Option<String>,
    pub usage: Option<AssistanceUsage>,
    pub latency_ms: Option<u64>,
    pub estimated_cost_microusd: Option<u64>,
    pub pricing_version: Option<String>,
}

impl AssistanceResult {
    pub fn assistance_gap(reason: AssistanceReason) -> Self {
        Self {
            disposition: match reason {
                AssistanceReason::Refused => AssistanceDisposition::Refused,
                AssistanceReason::Malformed
                | AssistanceReason::MissingAnswers
                | AssistanceReason::InvalidRequest => AssistanceDisposition::Invalid,
                AssistanceReason::ConfigurationChanged | AssistanceReason::Superseded => {
                    AssistanceDisposition::Stale
                }
                _ => AssistanceDisposition::Unavailable,
            },
            reason,
            answers: BTreeMap::new(),
            returned_model: None,
            usage: None,
            latency_ms: None,
            estimated_cost_microusd: None,
            pricing_version: None,
        }
    }

    /// Revalidate even injected adapters at the application/store boundary.
    /// The whole result abstains on a missing, refused or invalid answer.
    pub fn validate_result(&mut self, contract: &AssistanceContract) {
        if !matches!(
            self.disposition,
            AssistanceDisposition::Suggested | AssistanceDisposition::Uncertain
        ) {
            self.answers.clear();
            return;
        }
        let reason = if self.answers.keys().ne(contract.questions.keys()) {
            if self
                .answers
                .keys()
                .all(|k| contract.questions.contains_key(k))
            {
                Some(AssistanceReason::MissingAnswers)
            } else {
                Some(AssistanceReason::Malformed)
            }
        } else if self.answers.iter().any(|(name, a)| {
            let q = &contract.questions[name];
            if a.criterion_refs != q.criterion_refs()
                || a.probability.is_some_and(|p| !assistance_probability(p))
                || a.probabilities
                    .iter()
                    .any(|p| !assistance_probability(p.probability))
            {
                return true;
            }
            match (&a.value, q) {
                (AssistanceValue::Predicate(v), AssistanceQuestion::Predicate { .. }) => {
                    !a.probabilities.is_empty() || a.probability.is_none_or(|p| *v != (p >= 0.5))
                }
                (AssistanceValue::Choice(v), AssistanceQuestion::Choice { allowed, .. }) => {
                    !allowed.contains(v)
                        || !choice_is_consistent(v, a.probability, &a.probabilities)
                        || a.probabilities
                            .iter()
                            .map(|p| &p.value)
                            .collect::<BTreeSet<_>>()
                            != allowed.iter().collect::<BTreeSet<_>>()
                        || a.probabilities.len() != allowed.len()
                        || (a.probabilities.iter().map(|p| p.probability).sum::<f64>() - 1.0).abs()
                            > 0.001
                }
                _ => true,
            }
        }) {
            Some(AssistanceReason::Malformed)
        } else {
            None
        };
        if let Some(reason) = reason {
            *self = Self::assistance_gap(reason);
            return;
        }
        self.reason = AssistanceReason::Complete;
        self.disposition = if self.answers.iter().all(|(name, a)| {
            let q = &contract.questions[name];
            let confidence = a.probability.map(|p| match a.value {
                AssistanceValue::Predicate(false) => 1.0 - p,
                _ => p,
            });
            let abstains = matches!((q, &a.value),
                (AssistanceQuestion::Choice { abstain_values, .. }, AssistanceValue::Choice(v)) if abstain_values.contains(v));
            !abstains && q.min_confidence().zip(confidence).is_some_and(|(min, p)| p >= min)
        }) { AssistanceDisposition::Suggested } else { AssistanceDisposition::Uncertain };
    }
}

/// Largest allowed gap between a choice's confidence and the chosen value's
/// probability, and between that probability and the most probable value.
const CHOICE_CONFIDENCE_TOLERANCE: f64 = 0.01;

/// The confidence must match the chosen value's probability, and no value may be
/// more probable than it, both within `CHOICE_CONFIDENCE_TOLERANCE`.
fn choice_is_consistent(
    chosen: &str,
    confidence: Option<f64>,
    probabilities: &[AssistanceProbability],
) -> bool {
    let Some(confidence) = confidence else {
        return false;
    };
    let Some(chosen_probability) = probabilities
        .iter()
        .find(|p| p.value == chosen)
        .map(|p| p.probability)
    else {
        return false;
    };
    let most_probable = probabilities
        .iter()
        .map(|p| p.probability)
        .fold(f64::NEG_INFINITY, f64::max);
    (confidence - chosen_probability).abs() <= CHOICE_CONFIDENCE_TOLERANCE
        && most_probable - chosen_probability <= CHOICE_CONFIDENCE_TOLERANCE
}
