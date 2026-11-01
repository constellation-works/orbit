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

mod agent_request;
mod ci_evidence;
mod code_scanning;
mod desktop;
mod publication;
mod record;
mod recovery;
mod request;

pub use agent_request::{
    AGENT_DECISION_MAX_OPTIONS, AGENT_DECISION_MIN_OPTIONS, AgentDecisionOptionInput,
    AgentDecisionRequestError, agent_decision_answers, normalize_agent_decision_topic,
};
pub use ci_evidence::{CiFailureIdentity, CiFailureIdentityError, CiFailureSource};
pub use code_scanning::{CodeScanningAlertEvidence, CodeScanningAlertEvidenceError};
pub use desktop::{DesktopMutationReceipt, DesktopMutationReceiptError};
pub use publication::{
    PublicationHistoryError, PublicationProofContainer, PublicationProofValue,
    PublicationPushCheckpoint, PublicationPushDecision, PublicationPushPerformed,
    PublicationPushProof, PublicationSettlingDecision, PublicationSettlingObservation,
};
pub use record::{
    FINAL_RECOVERY_DECIDED_EVENT, JobStepInvocation, PILOT_ADMISSION_DECIDED_EVENT, TaskDecision,
};
pub use recovery::{PilotAdmissionVerdict, RecordedFinalRecovery};
pub use request::{
    DECISION_ANSWERED_EVENT, DECISION_DIGEST_EXPIRY_WINDOW_HOURS, DECISION_REOPENED_EVENT,
    DECISION_REQUESTED_EVENT, DECISION_TOKEN_CONSUMED_EVENT, DEFAULT_DECISION_REQUEST_TTL_HOURS,
    DecisionAction, DecisionAnswer, DecisionBinding, DecisionGate, DecisionOption,
    DecisionOptionError, DecisionOptionInput, DecisionRequest, DecisionRequestState,
    DecisionResolution, DecisionRewrite, PRECEDENT_ACTOR, PRECEDENT_BOOKKEEPING_NOTE,
    PrecedentCitation, PrecedentSource, current_candidate_identity, decision_option_answers,
    decision_requests_from_history, is_precedent_bookkeeping, open_decision_requests,
};
