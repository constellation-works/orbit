//! Options of an agent-raised decision request: two to four answers, one
//! recommended and listed first, each with a one-line label and consequence.

use serde::{Deserialize, Serialize};

use super::request::DecisionAnswer;

/// Fewest options an agent-raised decision request offers.
pub const AGENT_DECISION_MIN_OPTIONS: usize = 2;
/// Most options an agent-raised decision request offers.
pub const AGENT_DECISION_MAX_OPTIONS: usize = 4;
/// Longest label or consequence, in characters: both are one line.
const MAX_LINE_CHARS: usize = 240;
/// Longest topic, in characters.
const MAX_TOPIC_CHARS: usize = 64;

/// Why a proposed agent decision request is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentDecisionRequestError {
    /// The set does not hold two to four options.
    #[error("a decision request needs two to four options, not {0}")]
    OptionCount(usize),
    /// Not exactly one option is marked recommended.
    #[error("exactly one option must be marked `recommended: true`, not {0}")]
    RecommendedCount(usize),
    /// A text field is empty.
    #[error("{0} must not be empty")]
    EmptyField(String),
    /// A label or consequence spans lines or is too long.
    #[error("{0} must be one line of at most 240 characters")]
    NotOneLine(String),
    /// The topic is not a short token.
    #[error(
        "`topic` must be 1-64 characters of letters, digits and `.`, `_`, `:`, `/`, `-`, starting with a letter or digit"
    )]
    Topic,
}

/// One option as an agent proposes it, before validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentDecisionOptionInput {
    /// One-line label of the option.
    pub label: String,
    /// One-line consequence of choosing it.
    pub consequence: String,
    /// Whether the agent recommends this option. Exactly one option does.
    #[serde(default)]
    pub recommended: bool,
}

/// Validate a proposed option set and render it as answers `1`, `2`, ...
/// with the recommended option first. Returns the answers and the id of the
/// recommended one (always `1`).
pub fn agent_decision_answers(
    options: &[AgentDecisionOptionInput],
) -> Result<(Vec<DecisionAnswer>, String), AgentDecisionRequestError> {
    if !(AGENT_DECISION_MIN_OPTIONS..=AGENT_DECISION_MAX_OPTIONS).contains(&options.len()) {
        return Err(AgentDecisionRequestError::OptionCount(options.len()));
    }
    let recommended = options.iter().filter(|option| option.recommended).count();
    if recommended != 1 {
        return Err(AgentDecisionRequestError::RecommendedCount(recommended));
    }
    let mut ordered: Vec<&AgentDecisionOptionInput> = options.iter().collect();
    ordered.sort_by_key(|option| !option.recommended);
    let answers = ordered
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let number = index + 1;
            let label = one_line(&option.label, &format!("option {number} label"))?;
            let consequence =
                one_line(&option.consequence, &format!("option {number} consequence"))?;
            Ok(DecisionAnswer::plain(
                number.to_string(),
                label,
                consequence,
            ))
        })
        .collect::<Result<Vec<_>, AgentDecisionRequestError>>()?;
    Ok((answers, "1".to_string()))
}

fn one_line(value: &str, field: &str) -> Result<String, AgentDecisionRequestError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AgentDecisionRequestError::EmptyField(field.to_string()));
    }
    if value.contains(['\n', '\r']) || value.chars().count() > MAX_LINE_CHARS {
        return Err(AgentDecisionRequestError::NotOneLine(field.to_string()));
    }
    Ok(value.to_string())
}

/// A request's structured topic, trimmed and checked: a short token an exact
/// comparison can match, such as a corpus rule id.
pub fn normalize_agent_decision_topic(topic: &str) -> Result<String, AgentDecisionRequestError> {
    let topic = topic.trim();
    let mut chars = topic.chars();
    let valid = chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && topic.chars().count() <= MAX_TOPIC_CHARS
        && topic
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'));
    if valid {
        Ok(topic.to_string())
    } else {
        Err(AgentDecisionRequestError::Topic)
    }
}
