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
use crate::task::{DecisionOptionInput, decision_option_answers};

/// Name of the shipped activity that produces a [`FinalRecoveryDecision`].
pub const FINAL_RECOVERY_ACTIVITY: &str = "final_recovery";

/// The final-recovery activity's `crew_config_key`, and the key its frozen
/// crew draw is recorded under in pipeline state. Shipped job definitions and
/// run state persist it, so it keeps the spelling of the configuration key it
/// was named after; the pool itself is configured as
/// [`FINAL_RECOVERY_CREWS_CONFIG_KEY`].
pub const FINAL_RECOVERY_CREWS_KEY: &str = "workflow.final_recovery_crews";

/// Configuration key naming the weighted crew pool final recovery draws from.
pub const FINAL_RECOVERY_CREWS_CONFIG_KEY: &str = "workflow.purpose_crews.final_recovery";

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
    /// Only a human action outside Orbit unblocks the task. The applier
    /// blocks it and opens a decision request carrying `summary`, the exact
    /// `human_action` and one "Done — requeue" answer.
    Escalate {
        diagnosis: String,
        human_action: String,
        /// One or two plain sentences for the card; the diagnosis's opening
        /// stands in when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
    /// A human has to choose. The applier blocks the task and opens a
    /// decision request with these options, each a one-click answer.
    Decide {
        /// One or two plain-language sentences naming the decision needed,
        /// readable without opening the code.
        summary: String,
        /// Two or three options.
        options: Vec<DecisionOptionInput>,
        /// 1-based index of the recommended option.
        recommended: usize,
    },
    /// Mechanical: resume the blocking run from its checkpoints through the
    /// supported resume path. Once per run, counted in the requeue bound.
    ResumeRun { run_id: String, reason: String },
    /// Mechanical: re-request the failed required checks of the task's pull
    /// request at `head_sha`, then requeue it. Once per head, counted in the
    /// requeue bound.
    RerunCi { head_sha: String, reason: String },
    /// An Orbit defect, not a task problem: link an existing friction record
    /// (`friction_id`) or file one (`friction_title` and `friction_body`),
    /// and escalate with the exact human action.
    FrictionReported {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        friction_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        friction_title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        friction_body: Option<String>,
        human_action: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
    },
}

impl FinalRecoveryDecision {
    /// Every `decision` tag, in contract order.
    pub const KINDS: [&'static str; 10] = [
        "resume",
        "complete_no_diff",
        "reject",
        "archive",
        "requeue",
        "escalate",
        "decide",
        "resume_run",
        "rerun_ci",
        "friction_reported",
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
            Self::Decide { .. } => "decide",
            Self::ResumeRun { .. } => "resume_run",
            Self::RerunCi { .. } => "rerun_ci",
            Self::FrictionReported { .. } => "friction_reported",
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
        decision.validate_shape()?;
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
            summary: None,
        }
    }

    /// The checks a derived `Deserialize` cannot make: option sets, optional
    /// text and the two friction forms.
    fn validate_shape(&self) -> Result<(), FinalRecoveryError> {
        let malformed = |reason: String| FinalRecoveryError::Malformed { reason };
        match self {
            Self::Decide {
                options,
                recommended,
                ..
            } => {
                decision_option_answers(options, *recommended)
                    .map_err(|error| malformed(error.to_string()))?;
                for option in options {
                    for text in [&option.summary, &option.consequence] {
                        if text.chars().count() > MAX_DECISION_TEXT_CHARS {
                            return Err(FinalRecoveryError::FieldTooLong { field: "options" });
                        }
                    }
                }
            }
            Self::Escalate {
                summary: Some(summary),
                ..
            }
            | Self::FrictionReported {
                summary: Some(summary),
                ..
            } => {
                if summary.trim().is_empty() {
                    return Err(FinalRecoveryError::EmptyField { field: "summary" });
                }
                if summary.chars().count() > MAX_DECISION_TEXT_CHARS {
                    return Err(FinalRecoveryError::FieldTooLong { field: "summary" });
                }
            }
            _ => {}
        }
        if let Self::FrictionReported {
            friction_id,
            friction_title,
            friction_body,
            ..
        } = self
        {
            let present = |value: &Option<String>| {
                value
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
            };
            match (
                present(friction_id),
                present(friction_title),
                present(friction_body),
            ) {
                (true, false, false) | (false, true, true) => {}
                _ => {
                    return Err(malformed(
                        "friction_reported needs either `friction_id`, or both \
                         `friction_title` and `friction_body`"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(())
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
                ..
            } => vec![("diagnosis", diagnosis), ("human_action", human_action)],
            Self::Decide { summary, .. } => vec![("summary", summary)],
            Self::ResumeRun { run_id, reason } => vec![("run_id", run_id), ("reason", reason)],
            Self::RerunCi { head_sha, reason } => {
                vec![("head_sha", head_sha), ("reason", reason)]
            }
            Self::FrictionReported { human_action, .. } => vec![("human_action", human_action)],
        }
    }
}

fn is_commit_id(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.chars().all(|c| c.is_ascii_hexdigit())
}
