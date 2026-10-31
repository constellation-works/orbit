//! Final-recovery outcomes and pilot admission verdicts.

use serde::{Deserialize, Serialize};

use super::request::DecisionAnswer;
use crate::task::TaskStatus;

/// What the final-recovery applier did with a decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecordedFinalRecovery {
    /// The task moved to `status` on the verified covering commit.
    Completed {
        status: TaskStatus,
        evidence_commit: String,
    },
    Rejected,
    Archived,
    Requeued,
    /// The task was blocked for a human; `reason` is the applier's own when
    /// it overrode the proposal.
    Escalated {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Nothing changed but the audit record.
    Refused {
        reason: String,
    },
}

impl RecordedFinalRecovery {
    /// Stable outcome name, as the decision comment's `outcome=` shows it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed { .. } => "completed",
            Self::Rejected => "rejected",
            Self::Archived => "archived",
            Self::Requeued => "requeued",
            Self::Escalated { .. } => "escalated",
            Self::Refused { .. } => "refused",
        }
    }
}

/// An operator's decision on a task-pilot finding.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PilotAdmissionVerdict {
    /// Admit the task despite the finding.
    ApproveAnyway,
    /// The finding no longer applies.
    Clear,
    /// The operator evaluated the requirement and attached the evaluation.
    Evaluated,
    /// The operator changed the acceptance criteria the finding assessed.
    Rescope,
}

impl PilotAdmissionVerdict {
    /// The verdict a decision comment's first line names, if it names one.
    pub fn from_header(header: &str) -> Option<Self> {
        match header.trim() {
            "task-pilot-admission: approve-anyway" => Some(Self::ApproveAnyway),
            "task-pilot-admission: clear" => Some(Self::Clear),
            "task-pilot-admission: evaluated" => Some(Self::Evaluated),
            _ => None,
        }
    }

    /// Whether the verdict releases a duplicate or already-landed finding.
    pub fn overrides_findings(self) -> bool {
        matches!(self, Self::ApproveAnyway | Self::Clear)
    }
}

impl PilotAdmissionVerdict {
    /// Stable verdict spelling used as a decision answer id.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ApproveAnyway => "approve_anyway",
            Self::Clear => "clear",
            Self::Evaluated => "evaluated",
            Self::Rescope => "rescope",
        }
    }

    /// Allowed pilot-admission answers for a hold that records every verdict,
    /// including `evaluated`.
    ///
    /// Duplicate and already-landed holds use
    /// [`Self::finding_override_decision_answers`]: `evaluated` does not
    /// override those findings, so offering it spends the request without a
    /// resolution. `rescope` edits criteria and is never an answer.
    pub fn decision_answers() -> Vec<DecisionAnswer> {
        Self::answers_for([Self::ApproveAnyway, Self::Clear, Self::Evaluated])
    }

    /// Answers that release a duplicate or already-landed finding.
    ///
    /// Only verdicts for which [`Self::overrides_findings`] is true. `evaluated`
    /// is omitted: accepting it records an answer and leaves the hold in place.
    pub fn finding_override_decision_answers() -> Vec<DecisionAnswer> {
        Self::answers_for([Self::ApproveAnyway, Self::Clear])
    }

    fn answers_for(verdicts: impl IntoIterator<Item = Self>) -> Vec<DecisionAnswer> {
        verdicts.into_iter().map(Self::decision_answer).collect()
    }

    fn decision_answer(self) -> DecisionAnswer {
        let (label, consequence) = match self {
            Self::ApproveAnyway => ("Approve anyway", "Admit the task despite the finding."),
            Self::Clear => ("Clear", "The finding no longer applies."),
            Self::Evaluated => (
                "Evaluated",
                "The operator evaluated the requirement and attached the evaluation.",
            ),
            Self::Rescope => ("Rescope", "The acceptance criteria changed."),
        };
        DecisionAnswer {
            id: self.as_str().to_string(),
            label: label.to_string(),
            consequence: consequence.to_string(),
        }
    }
}
