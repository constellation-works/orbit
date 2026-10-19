//! Decisions automation acts on, recorded as typed data on the task history
//! event that carries them.
//!
//! The note and any comment written beside a decision are for people; the
//! readers that act on a decision read [`TaskHistoryEntry::decision`] only.
//! Records written before the field existed carry the decision in text; each
//! reader keeps its own isolated legacy reader for them.
//!
//! A record a reader acts on through more than one consumer has one
//! `from_entry` reader beside its type, which reads the decision and consults
//! the record's legacy note reader only for an entry with no decision.
//!
//! [`TaskHistoryEntry::decision`]: super::TaskHistoryEntry::decision

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ContextFilesWidening, RegressionRiskMarks, TaskStatus};
use crate::workflow::{FlakyValidation, HeldCandidate, KnownRedValidation};

/// History event the final-recovery applier records its decision on.
pub const FINAL_RECOVERY_DECIDED_EVENT: &str = "final_recovery_decided";
/// History event an operator's task-pilot admission decision is recorded on
/// when no typed hold has its own resolution event.
pub const PILOT_ADMISSION_DECIDED_EVENT: &str = "pilot_admission_decided";

/// A decision recorded on a task history event.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskDecision {
    /// What final recovery decided for one run, and what its applier did.
    FinalRecovery {
        /// The run the decision belongs to.
        run_id: String,
        /// The decision kind the agent proposed.
        decision: String,
        /// What the applier did with it.
        outcome: RecordedFinalRecovery,
    },
    /// An operator's decision on the task-pilot assessment a receipt sealed.
    PilotAdmission {
        /// Operation id of the pilot receipt the decision answers.
        operation_id: String,
        /// What the operator decided.
        verdict: PilotAdmissionVerdict,
        /// The typed hold admission reported when the operator decided, when
        /// it reported one; its scope bounds what the decision resolves.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hold: Option<Value>,
    },
    /// An auto drain withheld approval of the task its pilot assessed.
    DrainApprovalHeld {
        /// Why the drain held it.
        classification: String,
    },
    /// Where a candidate an owner-local run held is, recorded on the
    /// `candidate_held` event.
    CandidateHeld(HeldCandidate),
    /// Selectors appended for paths an agent changed, recorded on the
    /// `context_files_widened` event.
    ContextFilesWidened(ContextFilesWidening),
    /// A required command that failed only with tests its base fails too,
    /// recorded on the `known_red_validation` event.
    KnownRedValidation(KnownRedValidation),
    /// Tests a candidate failed and then passed in one isolated replay,
    /// recorded on the `flaky_validation` event.
    FlakyValidation(FlakyValidation),
    /// Context files a prompt marked as regression-prone, recorded on the
    /// `regression_risk_marked` event.
    RegressionRiskMarked(RegressionRiskMarks),
}

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
