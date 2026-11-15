//! Final-recovery outcomes and pilot admission verdicts.
//!
//! An operator comment's first line is the only spelling of a verdict.
//! [`PilotAdmissionVerdict::from_header`] reads it and
//! [`PilotAdmissionOperatorRecord::comment`] writes it.

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

/// First-line prefix of an operator pilot-admission comment. The verdict
/// spelling follows the space. [`PilotAdmissionVerdict::from_header`] and
/// [`PilotAdmissionOperatorRecord::comment`] are the only readers and writer.
const PILOT_ADMISSION_COMMENT_PREFIX: &str = "task-pilot-admission: ";

/// Why a dashboard pilot-admission record was refused before any task was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PilotAdmissionRecordRefusal {
    /// The verdict was missing or not `clear` or `approve-anyway`.
    VerdictInvalid,
    /// Evidence was missing or blank.
    EvidenceRequired,
}

/// An operator's pilot-admission verdict and the evidence recorded with it.
///
/// Evidence is trimmed and non-empty. `rescope` has no comment header, so it
/// cannot be stored here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotAdmissionOperatorRecord {
    verdict: PilotAdmissionVerdict,
    evidence: String,
}

impl PilotAdmissionOperatorRecord {
    /// Dashboard input. `clear` and `approve-anyway` are accepted
    /// case-insensitively, and `_` is accepted for `-`. An invalid verdict is
    /// refused before missing evidence.
    pub fn from_operator_input(
        verdict: Option<&str>,
        evidence: Option<&str>,
    ) -> Result<Self, PilotAdmissionRecordRefusal> {
        let normalized = verdict
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .replace('_', "-");
        let verdict = match normalized.as_str() {
            "clear" => PilotAdmissionVerdict::Clear,
            "approve-anyway" => PilotAdmissionVerdict::ApproveAnyway,
            _ => return Err(PilotAdmissionRecordRefusal::VerdictInvalid),
        };
        Self::from_verdict(verdict, evidence.unwrap_or(""))
    }

    /// A typed verdict that has a comment header, with non-empty trimmed evidence.
    pub fn from_verdict(
        verdict: PilotAdmissionVerdict,
        evidence: &str,
    ) -> Result<Self, PilotAdmissionRecordRefusal> {
        if verdict.comment_spelling().is_none() {
            return Err(PilotAdmissionRecordRefusal::VerdictInvalid);
        }
        let evidence = evidence.trim();
        if evidence.is_empty() {
            return Err(PilotAdmissionRecordRefusal::EvidenceRequired);
        }
        Ok(Self {
            verdict,
            evidence: evidence.to_string(),
        })
    }

    /// Spelling a dashboard response returns: `clear` or `approve-anyway`.
    pub fn verdict_label(&self) -> &'static str {
        self.verdict
            .comment_spelling()
            .unwrap_or_else(|| self.verdict.as_str())
    }

    /// The canonical human comment: the header [`PilotAdmissionVerdict::from_header`]
    /// recognizes, then the evidence.
    pub fn comment(&self) -> String {
        let spelling = self
            .verdict
            .comment_spelling()
            .unwrap_or_else(|| self.verdict.as_str());
        format!(
            "{PILOT_ADMISSION_COMMENT_PREFIX}{spelling}\n{}",
            self.evidence
        )
    }
}

impl PilotAdmissionVerdict {
    /// The verdict a decision comment's first line names, if it names one.
    pub fn from_header(header: &str) -> Option<Self> {
        let spelling = header.trim().strip_prefix(PILOT_ADMISSION_COMMENT_PREFIX)?;
        [Self::ApproveAnyway, Self::Clear, Self::Evaluated]
            .into_iter()
            .find(|verdict| verdict.comment_spelling() == Some(spelling))
    }

    /// Verdict word after [`PILOT_ADMISSION_COMMENT_PREFIX`]. `rescope` edits
    /// criteria and is never a comment header.
    fn comment_spelling(self) -> Option<&'static str> {
        match self {
            Self::ApproveAnyway => Some("approve-anyway"),
            Self::Clear => Some("clear"),
            Self::Evaluated => Some("evaluated"),
            Self::Rescope => None,
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
        DecisionAnswer::plain(self.as_str(), label, consequence)
    }
}
