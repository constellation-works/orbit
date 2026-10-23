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

use super::{ContextFilesWidening, RegressionRiskMarks, TaskHistoryEntry, TaskStatus};
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
    /// Idempotency receipt for a desktop mutation or its completed PR closure pass.
    DesktopMutation(DesktopMutationReceipt),
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
    /// A verified source-repair publication read and its durable wait reservation.
    SourceRepairPublication(PublicationSettlingObservation),
    /// Code scanning alert evidence a per-alert sweep task was filed from,
    /// recorded on its `created` event.
    CodeScanningAlert(CodeScanningAlertEvidence),
}

/// Durable identity and payload binding of an applied desktop mutation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopMutationReceipt {
    /// Client request identity, compared exactly on replay.
    pub request_id: String,
    /// Digest of the redacted request payload. Empty only for a legacy
    /// note-only PR closure marker, which recorded no payload binding.
    pub payload_digest: String,
}

/// A desktop history decision cannot be used as an idempotency receipt.
#[derive(Debug, thiserror::Error)]
pub enum DesktopMutationReceiptError {
    /// A different typed decision is authoritative and cannot be read as text.
    #[error("desktop history carries a mismatched decision")]
    MismatchedDecision,
    /// A legacy receipt identifies this request but has no readable digest.
    #[error("request identity reused with a different payload")]
    PayloadMismatch,
}

impl DesktopMutationReceipt {
    /// Read this request's receipt. Typed decisions take precedence even
    /// when their note names another request; a foreign decision never
    /// falls through to legacy text. Callers select the history event.
    pub fn from_entry(
        entry: &TaskHistoryEntry,
        request_id: &str,
    ) -> Result<Option<Self>, DesktopMutationReceiptError> {
        match &entry.decision {
            Some(TaskDecision::DesktopMutation(receipt)) => {
                Ok((receipt.request_id == request_id).then(|| receipt.clone()))
            }
            Some(_) => Err(DesktopMutationReceiptError::MismatchedDecision),
            None => Self::from_legacy_note(entry, request_id),
        }
    }

    /// The single compatibility reader for pre-typed mutation receipts and
    /// PR closure markers. Preserve the original exact note binding: any
    /// suffix other than the expected digest still refuses mutation replay.
    fn from_legacy_note(
        entry: &TaskHistoryEntry,
        request_id: &str,
    ) -> Result<Option<Self>, DesktopMutationReceiptError> {
        let Some(note) = entry.note.as_deref() else {
            return Ok(None);
        };
        let key = format!("desktop_request={request_id}");
        let payload_digest = if entry.event == "desktop_pr_closure" {
            if note != key {
                return Ok(None);
            }
            String::new()
        } else {
            let Some(suffix) = note.strip_prefix(&format!("{key} ")) else {
                return Ok(None);
            };
            suffix
                .strip_prefix("digest=")
                .ok_or(DesktopMutationReceiptError::PayloadMismatch)?
                .to_string()
        };
        Ok(Some(Self {
            request_id: request_id.into(),
            payload_digest,
        }))
    }
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

/// The result of a source-repair publication observation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationSettlingDecision {
    /// Reserve a bounded delay before reading again.
    Waiting,
    /// The candidate is published with all predicates satisfied.
    Settled,
    /// Publication could not be confirmed within the admitted predicates/budget.
    Refused,
}

/// Safe evidence retained in a push proof. These are the existing redacted,
/// bounded diagnostic representations, not raw provider/checkpoint payloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PublicationProofValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    Text(String),
    Truncated {
        prefix: String,
        bytes: usize,
        truncated: bool,
    },
    Container(PublicationProofContainer),
}

/// Fixed-depth markers used by the diagnostic projection of hostile values.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicationProofContainer {
    Object {
        oid: Option<Box<PublicationProofValue>>,
    },
    Array,
    Invalid,
}

impl PublicationProofValue {
    /// Only an ordinary projected string can establish an exact identity.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

/// Successful replacement outcomes accepted by the publication predicate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPushPerformed {
    PerformedForceWithLease,
    PerformedFastForward,
}

/// A push outcome, including safe evidence for unknown or malformed outcomes.
/// Unknown evidence is retained for equality and diagnostics, never authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum PublicationPushDecision {
    Performed(PublicationPushPerformed),
    Other(PublicationProofValue),
}

/// A successful step output authenticated against this run's push checkpoint.
/// All projected fields participate in equality across interrupted waits.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicationPushProof {
    pub step_index: u32,
    pub checkpoint: PublicationPushCheckpoint,
    pub checkpoint_sha256: String,
    pub branch: PublicationProofValue,
    pub local_sha: PublicationProofValue,
    pub remote_sha_before: PublicationProofValue,
    pub force_with_lease: Option<bool>,
    pub decision: PublicationPushDecision,
}

/// The named pipeline checkpoint that authenticated a publication push.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPushCheckpoint {
    Push,
}

/// Replay state recorded alongside the unchanged human diagnostic note.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicationSettlingObservation {
    pub run_id: PublicationProofValue,
    pub published_head: PublicationProofValue,
    pub successful_push: Option<PublicationPushProof>,
    pub reserved_delay_seconds: u64,
    pub decision: PublicationSettlingDecision,
}

/// A publication history record cannot safely restore a reservation.
#[derive(Debug, thiserror::Error)]
pub enum PublicationHistoryError {
    #[error("publication settling history unreadable")]
    History,
    #[error("publication settling reservation unreadable")]
    Reservation,
    #[error("publication settling head unreadable")]
    Head,
}

impl PublicationSettlingObservation {
    /// Read a matching typed decision, consulting the legacy JSON note only
    /// when no decision exists. Foreign runs cannot spend this run's budget.
    /// A mismatched decision refuses rather than falling through to prose.
    pub fn from_entry(
        entry: &TaskHistoryEntry,
        run: &str,
    ) -> Result<Option<Self>, PublicationHistoryError> {
        let observation = match &entry.decision {
            Some(TaskDecision::SourceRepairPublication(observation)) => {
                if observation.run_id.as_text() != Some(run) {
                    return Ok(None);
                }
                observation.clone()
            }
            Some(_) => return Err(PublicationHistoryError::History),
            None => return Self::from_legacy_note(entry.note.as_deref().unwrap_or(""), run),
        };
        observation.validate()?;
        Ok(Some(observation))
    }

    fn validate(&self) -> Result<(), PublicationHistoryError> {
        if self.reserved_delay_seconds > 0 && self.published_head.as_text().is_none() {
            return Err(PublicationHistoryError::Head);
        }
        Ok(())
    }

    /// Isolated compatibility boundary for the original note-only records.
    /// Keep the old run check before reservation validation: unrelated runs
    /// need not have readable reservation fields for this run to replay.
    fn from_legacy_note(note: &str, run: &str) -> Result<Option<Self>, PublicationHistoryError> {
        let value: Value =
            serde_json::from_str(note).map_err(|_| PublicationHistoryError::History)?;
        if value["run_id"] != run {
            return Ok(None);
        }
        let reserved_delay_seconds = value["settling"]["reserved_delay_seconds"]
            .as_u64()
            .ok_or(PublicationHistoryError::Reservation)?;
        let published_head = serde_json::from_value(value["published_head"].clone())
            .map_err(|_| PublicationHistoryError::Head)?;
        let decision = serde_json::from_value(value["settling"]["decision"].clone())
            .unwrap_or(PublicationSettlingDecision::Refused);
        let successful_push = if reserved_delay_seconds > 0 {
            serde_json::from_value(value["successful_push"].clone())
                .map_err(|_| PublicationHistoryError::History)?
        } else {
            None
        };
        let observation = Self {
            run_id: PublicationProofValue::Text(run.into()),
            published_head,
            successful_push,
            reserved_delay_seconds,
            decision,
        };
        observation.validate()?;
        Ok(Some(observation))
    }
}

/// Typed Code scanning alert evidence carried on a per-alert task's
/// creation history. The task description renders the same fields for
/// people; consolidation matches on this struct, never on that prose.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CodeScanningAlertEvidence {
    pub repository: String,
    pub number: u64,
    pub rule_id: String,
    pub rule_name: String,
    pub security_severity: String,
    pub tool_name: String,
    pub tool_version: String,
    pub tool_guid: String,
    pub message: String,
    pub git_ref: String,
    pub commit_sha: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
    pub html_url: String,
}

/// A creation history entry carries a decision that is not Code scanning
/// alert evidence, so no legacy text may stand in for it.
#[derive(Debug, thiserror::Error)]
#[error("creation history carries a mismatched decision")]
pub struct CodeScanningAlertEvidenceError;

impl CodeScanningAlertEvidence {
    /// Read the evidence one history entry carries. `Ok(None)` means the
    /// entry carries no decision, so a caller may consult its isolated
    /// legacy reader; a mismatched decision is an error and must never
    /// fall through to text.
    pub fn from_entry(
        entry: &TaskHistoryEntry,
    ) -> Result<Option<Self>, CodeScanningAlertEvidenceError> {
        match &entry.decision {
            Some(TaskDecision::CodeScanningAlert(evidence)) => Ok(Some(evidence.clone())),
            Some(_) => Err(CodeScanningAlertEvidenceError),
            None => Ok(None),
        }
    }

    /// Read the evidence a task's creation history carries: the decision
    /// on its `created` entry. Later entries carry other decisions and
    /// are not consulted. `Ok(None)` when that entry (or the history)
    /// carries no decision.
    pub fn from_history(
        history: &[TaskHistoryEntry],
    ) -> Result<Option<Self>, CodeScanningAlertEvidenceError> {
        let Some(created) = history.iter().find(|entry| entry.event == "created") else {
            return Ok(None);
        };
        Self::from_entry(created)
    }
}
