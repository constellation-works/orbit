//! Decision answers, option payloads and rewrite/action validation.

use serde::{Deserialize, Serialize};

/// Risk of an answer. Unknown is conservative for absent or future values.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRiskTier {
    /// No lasting effect.
    Routine,
    /// Reversible through the normal path.
    Reversible,
    /// Cannot be undone through the normal path.
    Irreversible,
    /// Widens authority.
    Authority,
    /// Commits additional spend.
    Spend,
    /// Undeclared or unrecognised.
    #[default]
    Unknown,
}

impl<'de> Deserialize<'de> for DecisionRiskTier {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(match value.as_str() {
            Some("routine") => Self::Routine,
            Some("reversible") => Self::Reversible,
            Some("irreversible") => Self::Irreversible,
            Some("authority") => Self::Authority,
            Some("spend") => Self::Spend,
            _ => Self::Unknown,
        })
    }
}

/// One allowed answer to a [`DecisionRequest`](super::model::DecisionRequest).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionAnswer {
    /// Stable answer id, taken from the vocabulary that raised the hold.
    pub id: String,
    /// Short label rendered beside the answer.
    pub label: String,
    /// One-line consequence of choosing this answer.
    pub consequence: String,
    /// Risk declared for this answer.
    #[serde(default)]
    pub risk_tier: DecisionRiskTier,
    /// What choosing the answer changes, when it is a one-click option:
    /// a field rewrite, an action, or both. Absent on approval and
    /// pilot-admission answers and on requests raised before options existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<DecisionOption>,
}

impl DecisionAnswer {
    /// An answer with no option payload: its gate defines what it does.
    pub fn plain(
        id: impl Into<String>,
        label: impl Into<String>,
        consequence: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            consequence: consequence.into(),
            option: None,
            risk_tier: DecisionRiskTier::Unknown,
        }
    }
}

/// The task fields a decision option rewrites. At least one is set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionRewrite {
    /// The complete new description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The complete new acceptance-criteria list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance_criteria: Option<Vec<String>>,
}

impl DecisionRewrite {
    /// Whether the rewrite changes no field.
    pub fn is_empty(&self) -> bool {
        self.description.is_none() && self.acceptance_criteria.is_none()
    }
}

/// The closed set of actions a decision option may run after its rewrite.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionAction {
    /// Return the task to `backlog` for a fresh run (approve a proposed task).
    Requeue,
    /// Merge the receipt-bound failure handoff as the operator.
    Land,
    /// Explicitly submit one new pilot (no rewrite required); a blocked task returns to backlog.
    RePilot,
    /// Assign `crew`, then return the task to `backlog`.
    ChangeCrew {
        /// The registered crew the task runs on next.
        crew: String,
    },
    /// File a follow-up task linked `spawned_from` this one; the option's
    /// rewrite narrows this task, which returns to `backlog`.
    Split {
        /// Title of the follow-up task.
        title: String,
        /// Description of the follow-up task.
        description: String,
        /// Acceptance criteria of the follow-up task.
        acceptance_criteria: Vec<String>,
    },
    /// Reject the task.
    Reject,
    /// Archive the task.
    Archive,
}

impl DecisionAction {
    /// Every action tag, in contract order.
    pub const KINDS: [&'static str; 7] = [
        "requeue",
        "land",
        "re_pilot",
        "change_crew",
        "split",
        "reject",
        "archive",
    ];

    /// The stable tag of this action.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Requeue => "requeue",
            Self::Land => "land",
            Self::RePilot => "re_pilot",
            Self::ChangeCrew { .. } => "change_crew",
            Self::Split { .. } => "split",
            Self::Reject => "reject",
            Self::Archive => "archive",
        }
    }
}

/// The payload of a one-click decision option.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionOption {
    /// One sentence naming what the option decides.
    pub summary: String,
    /// Field rewrite applied when the option is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<DecisionRewrite>,
    /// Action run when the option is chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<DecisionAction>,
}

/// Why a proposed set of decision options is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecisionOptionError {
    /// The set does not hold two or three options.
    #[error("a decision needs two or three options, not {0}")]
    OptionCount(usize),
    /// `recommended` names no option.
    #[error("`recommended` must name one of the {0} options (1-based)")]
    Recommended(usize),
    /// A text field is empty.
    #[error("{0} must not be empty")]
    EmptyField(String),
    /// An option has neither a rewrite nor an action.
    #[error("option {0} has neither a rewrite nor an action, so choosing it would change nothing")]
    NoEffect(usize),
    /// An option's action is not one of the closed set.
    #[error("option {index} has an unusable action: {reason}")]
    Action {
        /// 1-based option index.
        index: usize,
        /// What is wrong.
        reason: String,
    },
}

/// One option as an agent or tool proposes it, before validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DecisionOptionInput {
    /// One-sentence summary of the option.
    pub summary: String,
    /// One-line consequence of choosing it.
    pub consequence: String,
    /// Optional field rewrite.
    #[serde(default)]
    pub rewrite: Option<DecisionRewrite>,
    /// Optional action, tagged by `kind`; unknown kinds are refused when the
    /// input is parsed.
    #[serde(default)]
    pub action: Option<DecisionAction>,
}

/// Validate a proposed option set and render it as answers `1`, `2`, `3`.
/// Two or three options, every text non-empty, each with a rewrite, an
/// action or both, an action that carries what it needs, and `recommended`
/// (1-based) naming one of them.
pub fn decision_option_answers(
    options: &[DecisionOptionInput],
    recommended: usize,
) -> Result<Vec<DecisionAnswer>, DecisionOptionError> {
    if !(2..=3).contains(&options.len()) {
        return Err(DecisionOptionError::OptionCount(options.len()));
    }
    if !(1..=options.len()).contains(&recommended) {
        return Err(DecisionOptionError::Recommended(options.len()));
    }
    options
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let number = index + 1;
            let text = |value: &str, field: &str| {
                let value = value.trim();
                if value.is_empty() {
                    Err(DecisionOptionError::EmptyField(format!(
                        "option {number} {field}"
                    )))
                } else {
                    Ok(value.to_string())
                }
            };
            let summary = text(&option.summary, "summary")?;
            let consequence = text(&option.consequence, "consequence")?;
            let rewrite = match &option.rewrite {
                Some(rewrite) => {
                    Some(validated_rewrite(number, rewrite)?).filter(|r| !r.is_empty())
                }
                None => None,
            };
            let action = match &option.action {
                Some(action) => Some(validated_action(number, action)?),
                None => None,
            };
            if rewrite.is_none() && action.is_none() {
                return Err(DecisionOptionError::NoEffect(number));
            }
            if rewrite.is_some()
                && matches!(
                    action,
                    Some(DecisionAction::Reject | DecisionAction::Archive)
                )
            {
                return Err(DecisionOptionError::Action {
                    index: number,
                    reason: "reject and archive end the task, so they take no rewrite".to_string(),
                });
            }
            Ok(DecisionAnswer {
                id: number.to_string(),
                label: summary.clone(),
                consequence,
                risk_tier: DecisionRiskTier::Unknown,
                option: Some(DecisionOption {
                    summary,
                    rewrite,
                    action,
                }),
            })
        })
        .collect()
}

fn validated_rewrite(
    number: usize,
    rewrite: &DecisionRewrite,
) -> Result<DecisionRewrite, DecisionOptionError> {
    let description = match &rewrite.description {
        Some(value) if value.trim().is_empty() => {
            return Err(DecisionOptionError::EmptyField(format!(
                "option {number} rewrite.description"
            )));
        }
        Some(value) => Some(value.trim().to_string()),
        None => None,
    };
    let acceptance_criteria = match &rewrite.acceptance_criteria {
        Some(criteria) => {
            let criteria: Vec<String> = criteria
                .iter()
                .map(|criterion| criterion.trim().to_string())
                .collect();
            if criteria.is_empty() || criteria.iter().any(String::is_empty) {
                return Err(DecisionOptionError::EmptyField(format!(
                    "option {number} rewrite.acceptance_criteria"
                )));
            }
            Some(criteria)
        }
        None => None,
    };
    Ok(DecisionRewrite {
        description,
        acceptance_criteria,
    })
}

fn validated_action(
    number: usize,
    action: &DecisionAction,
) -> Result<DecisionAction, DecisionOptionError> {
    let bad = |reason: &str| DecisionOptionError::Action {
        index: number,
        reason: reason.to_string(),
    };
    Ok(match action {
        DecisionAction::Land => {
            return Err(bad(
                "land is offered by the operator's failure-handoff decision, not proposed as an option",
            ));
        }
        DecisionAction::ChangeCrew { crew } => {
            if crew.trim().is_empty() {
                return Err(bad("change_crew needs a crew"));
            }
            DecisionAction::ChangeCrew {
                crew: crew.trim().to_string(),
            }
        }
        DecisionAction::Split {
            title,
            description,
            acceptance_criteria,
        } => {
            if title.trim().is_empty() || description.trim().is_empty() {
                return Err(bad("split needs a title and a description"));
            }
            let criteria: Vec<String> = acceptance_criteria
                .iter()
                .map(|criterion| criterion.trim().to_string())
                .collect();
            if criteria.is_empty() || criteria.iter().any(String::is_empty) {
                return Err(bad("split needs acceptance criteria"));
            }
            DecisionAction::Split {
                title: title.trim().to_string(),
                description: description.trim().to_string(),
                acceptance_criteria: criteria,
            }
        }
        other => other.clone(),
    })
}
