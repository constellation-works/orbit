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

use super::{
    ContextFilesWidening, RegressionRiskMarks, TagsChanged, Task, TaskHistoryEntry, TaskStatus,
};
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
    /// The tag lists before and after a write that changed them, recorded
    /// on the `tags_changed` event.
    TagsChanged(TagsChanged),
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

/// History event that raises a [`DecisionRequest`].
pub const DECISION_REQUESTED_EVENT: &str = "decision_requested";
/// History event that records an applied answer to a [`DecisionRequest`].
pub const DECISION_ANSWERED_EVENT: &str = "decision_answered";
/// History event that records a decision token consumed by a refused
/// submission that had already authenticated (design
/// `docs/design/human-decision-channel/2_design.md` §2.3). The note names
/// the request and the token's SHA-256 fingerprint, never the token.
pub const DECISION_TOKEN_CONSUMED_EVENT: &str = "decision_token_consumed";
/// Default lifetime of a decision request, in hours.
pub const DEFAULT_DECISION_REQUEST_TTL_HOURS: i64 = 72;

/// The gate a [`DecisionRequest`] waits on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionGate {
    /// A blocked-task triage proposal.
    BlockTriage,
    /// A task-pilot admission hold.
    PilotAdmission,
    /// Approval of a proposed task.
    ProposedApproval,
    /// Approval of a task in review.
    ReviewApproval,
}

impl DecisionGate {
    /// The gate a stable gate name denotes, if it denotes one.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "block_triage" => Some(Self::BlockTriage),
            "pilot_admission" => Some(Self::PilotAdmission),
            "proposed_approval" => Some(Self::ProposedApproval),
            "review_approval" => Some(Self::ReviewApproval),
            _ => None,
        }
    }

    /// Stable gate name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BlockTriage => "block_triage",
            Self::PilotAdmission => "pilot_admission",
            Self::ProposedApproval => "proposed_approval",
            Self::ReviewApproval => "review_approval",
        }
    }
}

/// Lifecycle state of a [`DecisionRequest`]. `Superseded` and `Expired`
/// are derived on read; only `Open` is written when a request is raised.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionRequestState {
    /// Raised, unanswered, binding still current.
    Open,
    /// An allowed answer was applied; terminal.
    Answered,
    /// The binding stopped matching before an answer arrived; terminal.
    Superseded,
    /// The expiry passed while open; terminal.
    Expired,
}

impl DecisionRequestState {
    /// Stable state name.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Answered => "answered",
            Self::Superseded => "superseded",
            Self::Expired => "expired",
        }
    }
}

/// One allowed answer to a [`DecisionRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionAnswer {
    /// Stable answer id, taken from the vocabulary that raised the hold.
    pub id: String,
    /// Short label rendered beside the answer.
    pub label: String,
    /// One-line consequence of choosing this answer.
    pub consequence: String,
}

/// Stale-guard binding of a [`DecisionRequest`]: the episode, operation or
/// revision the request was raised against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DecisionBinding {
    /// Block episode key (the `episode=` a triage marker records).
    BlockEpisode {
        /// The episode key.
        episode: String,
    },
    /// Pilot `operation_id` of the holding assessment.
    PilotOperation {
        /// The pilot receipt operation id.
        operation_id: String,
    },
    /// Task revision and status an approval request was raised against.
    TaskRevision {
        /// The task `updated_at` when the request was raised.
        revision: String,
        /// The task status when the request was raised.
        status: TaskStatus,
    },
}

impl DecisionBinding {
    /// The value a newer binding is compared against for supersession.
    pub fn key(&self) -> String {
        match self {
            Self::BlockEpisode { episode } => format!("block_episode:{episode}"),
            Self::PilotOperation { operation_id } => {
                format!("pilot_operation:{operation_id}")
            }
            Self::TaskRevision { revision, status } => {
                format!("task_revision:{revision}:{status}")
            }
        }
    }
}

/// What an applied answer recorded on a [`DecisionRequest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionResolution {
    /// Request the answer applies to. Redundant inside the request's own
    /// `resolution`, but a `decision_answered` entry carries only this
    /// record, so it names its request here: appended history entries do
    /// not retain their `operation_id` through the task store.
    #[serde(default)]
    pub request_id: String,
    /// Chosen answer id.
    pub answer_id: String,
    /// Human who answered.
    pub answered_by: String,
    /// Channel provenance, when the answer arrived through a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    /// Free-text note kept with the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// When the answer was applied.
    pub answered_at: chrono::DateTime<chrono::Utc>,
}

/// A request for a human decision on held work, stored as a
/// `decision_requested` task-history entry whose note carries this record
/// as JSON (the design names the entry's `operation_id` as the request
/// id; the task store does not retain that field on appended entries, so
/// the note record is authoritative). No new store: supersession and
/// expiry are derived on read, so they need no write.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DecisionRequest {
    /// Opaque request id (`dr-<random>`); the token binds to it.
    pub id: String,
    /// The held task.
    pub task_id: String,
    /// The gate the request waits on.
    pub gate: DecisionGate,
    /// Allowed answers, in the order surfaces present them.
    pub answers: Vec<DecisionAnswer>,
    /// Stale-guard binding the request was raised against.
    pub binding: DecisionBinding,
    /// Mechanism that raised it (`block_triage`, `task_pilot`, `digest`).
    pub raised_by: String,
    /// When it was raised.
    pub raised_at: chrono::DateTime<chrono::Utc>,
    /// `raised_at + request_ttl_hours`; the token expires with it.
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Stored state; effective state is derived by
    /// [`decision_requests_from_history`].
    pub state: DecisionRequestState,
    /// Recorded when the request is answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<DecisionResolution>,
}

impl DecisionRequest {
    /// A new open request with a generated id, expiring after the
    /// default TTL ([`DEFAULT_DECISION_REQUEST_TTL_HOURS`]).
    pub fn new(
        task_id: String,
        gate: DecisionGate,
        answers: Vec<DecisionAnswer>,
        binding: DecisionBinding,
        raised_by: String,
        raised_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let id = Self::new_id(&task_id, &binding, raised_at);
        Self {
            id,
            task_id,
            gate,
            answers,
            binding,
            raised_by,
            raised_at,
            expires_at: raised_at + chrono::Duration::hours(DEFAULT_DECISION_REQUEST_TTL_HOURS),
            state: DecisionRequestState::Open,
            resolution: None,
        }
    }

    /// Deterministic request id for one raise, from the task, binding and
    /// raise time, so a retried raise of the same hold reuses its id.
    pub fn new_id(
        task_id: &str,
        binding: &DecisionBinding,
        raised_at: chrono::DateTime<chrono::Utc>,
    ) -> String {
        use sha2::{Digest, Sha256};
        let seed = format!(
            "{task_id}|{}|{}",
            binding.key(),
            raised_at.timestamp_nanos_opt().unwrap_or_default()
        );
        format!(
            "dr-{}",
            crate::identity::lower_hex(Sha256::digest(seed.as_bytes()))
                .chars()
                .take(16)
                .collect::<String>()
        )
    }

    /// The history entry that raises this request, written by `by`.
    pub fn history_entry(&self, by: &str) -> TaskHistoryEntry {
        TaskHistoryEntry {
            at: self.raised_at,
            by: by.to_string(),
            event: DECISION_REQUESTED_EVENT.to_string(),
            note: serde_json::to_string(self).ok(),
            from_status: None,
            to_status: None,
            operation_id: Some(self.id.clone()),
            decision: None,
        }
    }

    /// Read the request one history entry carries, or `None` when the
    /// entry is not a readable raise of one. The request id lives in the
    /// note record: appended history entries do not retain their
    /// `operation_id` through the task store, so a present one is only
    /// checked, never required.
    pub fn from_entry(entry: &TaskHistoryEntry) -> Option<Self> {
        if entry.event != DECISION_REQUESTED_EVENT {
            return None;
        }
        let note = entry.note.as_deref()?;
        let request: Self = serde_json::from_str(note).ok()?;
        if let Some(operation_id) = entry.operation_id.as_deref()
            && operation_id != request.id
        {
            return None;
        }
        Some(request)
    }

    /// Whether `answer_id` is one of this request's allowed answers.
    pub fn allows(&self, answer_id: &str) -> bool {
        self.answers.iter().any(|answer| answer.id == answer_id)
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

    /// Allowed pilot-admission answers for a decision request: every
    /// verdict except `rescope`, which edits criteria.
    pub fn decision_answers() -> Vec<DecisionAnswer> {
        [
            (
                Self::ApproveAnyway,
                "Approve anyway",
                "Admit the task despite the finding.",
            ),
            (Self::Clear, "Clear", "The finding no longer applies."),
            (
                Self::Evaluated,
                "Evaluated",
                "The operator evaluated the requirement and attached the evaluation.",
            ),
        ]
        .into_iter()
        .map(|(verdict, label, consequence)| DecisionAnswer {
            id: verdict.as_str().to_string(),
            label: label.to_string(),
            consequence: consequence.to_string(),
        })
        .collect()
    }
}

/// Every decision request in `history`, oldest first, with its effective
/// state: a request is `answered` when a `decision_answered` entry (or its
/// own resolution) records one, `expired` once its expiry passes, and
/// `superseded` when its binding is no longer current or a newer request
/// for the same task follows it. At most one request per task reads as `open`.
///
/// Supply the current task and its history. `pilot_operation` is the latest
/// assessment operation id only while its admission hold still applies;
/// pass `None` when the hold is cleared. Block episodes use the latest move
/// to `blocked`, with the same nanosecond timestamp key as block triage.
pub fn decision_requests_from_history(
    task: &Task,
    history: &[TaskHistoryEntry],
    pilot_operation: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<DecisionRequest> {
    let block_episode = (task.status == TaskStatus::Blocked)
        .then(|| {
            history
                .iter()
                .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
        })
        .flatten()
        .map(|entry| entry.at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true));
    let binding_current = |request: &DecisionRequest| {
        request.task_id == task.id
            && match &request.binding {
                DecisionBinding::BlockEpisode { episode } => {
                    block_episode.as_ref() == Some(episode)
                }
                DecisionBinding::PilotOperation { operation_id } => {
                    task.status == TaskStatus::Backlog
                        && pilot_operation == Some(operation_id.as_str())
                }
                DecisionBinding::TaskRevision { revision, status } => {
                    *status == task.status
                        && chrono::DateTime::parse_from_rfc3339(revision)
                            .is_ok_and(|revision| revision == task.updated_at)
                }
            }
    };
    let answered: std::collections::BTreeMap<String, DecisionResolution> = history
        .iter()
        .filter(|entry| entry.event == DECISION_ANSWERED_EVENT)
        .filter_map(|entry| {
            let resolution: DecisionResolution =
                serde_json::from_str(entry.note.as_deref()?).ok()?;
            let request_id = if resolution.request_id.is_empty() {
                entry.operation_id.as_deref()?.to_string()
            } else {
                resolution.request_id.clone()
            };
            Some((request_id, resolution))
        })
        .collect();
    let mut requests: Vec<DecisionRequest> = history
        .iter()
        .filter_map(DecisionRequest::from_entry)
        .collect();
    let task_ids: Vec<String> = requests
        .iter()
        .map(|request| request.task_id.clone())
        .collect();
    for (index, request) in requests.iter_mut().enumerate() {
        if let Some(resolution) = answered.get(&request.id) {
            request.state = DecisionRequestState::Answered;
            request.resolution = Some(resolution.clone());
        } else if request.resolution.is_some() || request.state == DecisionRequestState::Answered {
            request.state = DecisionRequestState::Answered;
        } else if !binding_current(request)
            || task_ids
                .iter()
                .skip(index + 1)
                .any(|task_id| task_id == &request.task_id)
        {
            request.state = DecisionRequestState::Superseded;
        } else if now >= request.expires_at {
            request.state = DecisionRequestState::Expired;
        } else {
            request.state = DecisionRequestState::Open;
        }
    }
    requests
}

/// The open decision requests in `task`'s history, oldest first.
/// Current binding inputs follow [`decision_requests_from_history`].
pub fn open_decision_requests(
    task: &Task,
    history: &[TaskHistoryEntry],
    pilot_operation: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<DecisionRequest> {
    decision_requests_from_history(task, history, pilot_operation, now)
        .into_iter()
        .filter(|request| request.state == DecisionRequestState::Open)
        .collect()
}
