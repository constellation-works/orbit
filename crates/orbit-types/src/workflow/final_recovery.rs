//! The decision a `final_recovery` activity returns for a task whose run
//! failed after step recovery was exhausted.
//!
//! The agent only proposes; a deterministic applier owns every lifecycle
//! write. The contract is therefore strict: any output that is not exactly one
//! of these decisions, with every field present and non-empty, is treated as
//! [`FinalRecoveryDecision::Escalate`], never as a best-effort guess.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::FinalRecoveryError;

/// Name of the shipped activity that produces a [`FinalRecoveryDecision`].
pub const FINAL_RECOVERY_ACTIVITY: &str = "final_recovery";

/// Configuration key naming the weighted crew pool final recovery draws from.
/// It is also the activity's `crew_config_key`.
pub const FINAL_RECOVERY_CREWS_KEY: &str = "workflow.final_recovery_crews";

/// Longest free-text field a decision may carry. Every decision is recorded
/// as a task comment, so an unbounded rationale is refused rather than stored.
pub const MAX_DECISION_TEXT_CHARS: usize = 8_000;

/// One final-recovery decision, tagged by `decision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum FinalRecoveryDecision {
    /// The worktree is repaired; rerun the pipeline from `step_id`.
    Resume { step_id: String, rationale: String },
    /// The task's outcome is already on the base branch at `evidence_commit`.
    CompleteNoDiff {
        evidence_commit: String,
        rationale: String,
    },
    /// The task should not be done as written.
    Reject { reason: String, evidence: String },
    /// The task is obsolete or superseded.
    Archive { reason: String },
    /// A verified change makes a fresh attempt worthwhile.
    Requeue { reason: String },
    /// A human has to act; nothing automated is safe.
    Escalate {
        diagnosis: String,
        human_action: String,
    },
}

impl FinalRecoveryDecision {
    /// Every `decision` tag, in contract order.
    pub const KINDS: [&'static str; 6] = [
        "resume",
        "complete_no_diff",
        "reject",
        "archive",
        "requeue",
        "escalate",
    ];

    /// The `decision` tag of this value.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Resume { .. } => "resume",
            Self::CompleteNoDiff { .. } => "complete_no_diff",
            Self::Reject { .. } => "reject",
            Self::Archive { .. } => "archive",
            Self::Requeue { .. } => "requeue",
            Self::Escalate { .. } => "escalate",
        }
    }

    /// Parse an activity result strictly: one known decision, no unknown
    /// fields, every text field non-empty and bounded, and a full or
    /// abbreviated hexadecimal `evidence_commit`.
    pub fn parse(output: &Value) -> Result<Self, FinalRecoveryError> {
        let decision =
            Self::deserialize(output).map_err(|error| FinalRecoveryError::Malformed {
                reason: error.to_string(),
            })?;
        for (field, value) in decision.text_fields() {
            let value = value.trim();
            if value.is_empty() {
                return Err(FinalRecoveryError::EmptyField { field });
            }
            if value.chars().count() > MAX_DECISION_TEXT_CHARS {
                return Err(FinalRecoveryError::FieldTooLong { field });
            }
        }
        if let Self::CompleteNoDiff {
            evidence_commit, ..
        } = &decision
            && !is_commit_id(evidence_commit.trim())
        {
            return Err(FinalRecoveryError::EvidenceCommit {
                evidence_commit: evidence_commit.clone(),
            });
        }
        Ok(decision)
    }

    /// The decision an activity result stands for. A missing or malformed
    /// result escalates, naming why, so a broken agent never moves a task.
    pub fn from_output(output: Option<&Value>) -> Self {
        let error = match output {
            Some(output) => match Self::parse(output) {
                Ok(decision) => return decision,
                Err(error) => error.to_string(),
            },
            None => "the activity returned no result".to_string(),
        };
        Self::Escalate {
            diagnosis: format!("final recovery returned a malformed decision: {error}"),
            human_action: "Inspect the failed run and its final-recovery output, then move the \
                           task by hand."
                .to_string(),
        }
    }

    fn text_fields(&self) -> Vec<(&'static str, &str)> {
        match self {
            Self::Resume { step_id, rationale } => {
                vec![("step_id", step_id), ("rationale", rationale)]
            }
            Self::CompleteNoDiff {
                evidence_commit,
                rationale,
            } => vec![
                ("evidence_commit", evidence_commit),
                ("rationale", rationale),
            ],
            Self::Reject { reason, evidence } => {
                vec![("reason", reason), ("evidence", evidence)]
            }
            Self::Archive { reason } | Self::Requeue { reason } => vec![("reason", reason)],
            Self::Escalate {
                diagnosis,
                human_action,
            } => vec![("diagnosis", diagnosis), ("human_action", human_action)],
        }
    }
}

fn is_commit_id(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}
