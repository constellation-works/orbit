//! Decision gates and their fixed answer vocabularies.

use serde::{Deserialize, Serialize};

use super::options::DecisionAnswer;

/// The gate a [`DecisionRequest`](super::model::DecisionRequest) waits on.
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
    /// A multiple-choice question an agent raised on a human-crew task. An
    /// answer records the chosen option and completes the task.
    AgentRequest,
    /// Human review of an answer the decision model applied: `confirm`
    /// keeps it and makes it precedent, `veto` re-opens the original
    /// request. The model never answers this gate. [ORB-22808] Decision
    /// assistance was removed: nothing raises this gate any more, and it
    /// stays so task history recorded before then still reads.
    ModelAnswerReview,
    /// A request to widen a resident mandate's authority, relayed by a
    /// caller without operator authority: `grant` applies exactly the
    /// requested addition, `reject` changes nothing. Only a human answers
    /// it; precedent and the decision model never do.
    ResidentAuthority,
    /// Consent to run one workspace script at one content digest with one
    /// capability set (script steps §2): `grant` writes exactly that grant
    /// with the answering human as grantor, `reject` writes nothing. Anyone
    /// may raise one; only a human answers it, never precedent or the model.
    ScriptGrant,
}

impl DecisionGate {
    /// The gate a stable gate name denotes, if it denotes one.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "block_triage" => Some(Self::BlockTriage),
            "pilot_admission" => Some(Self::PilotAdmission),
            "proposed_approval" => Some(Self::ProposedApproval),
            "review_approval" => Some(Self::ReviewApproval),
            "agent_request" => Some(Self::AgentRequest),
            "model_answer_review" => Some(Self::ModelAnswerReview),
            "resident_authority" => Some(Self::ResidentAuthority),
            "script_grant" => Some(Self::ScriptGrant),
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
            Self::AgentRequest => "agent_request",
            Self::ModelAnswerReview => "model_answer_review",
            Self::ResidentAuthority => "resident_authority",
            Self::ScriptGrant => "script_grant",
        }
    }

    /// The allowed answers of an approval request on this gate:
    /// `approve` / `reject`, in presentation order. Empty for the
    /// non-approval gates, whose answers come from the hold that
    /// raised them.
    pub fn approval_answers(&self) -> Vec<DecisionAnswer> {
        let approved = match self {
            Self::ProposedApproval => {
                "Approve the proposed task; it goes through task-pilot screening like a CLI approval."
            }
            Self::ReviewApproval => "Approve the task in review.",
            Self::BlockTriage
            | Self::PilotAdmission
            | Self::AgentRequest
            | Self::ModelAnswerReview
            | Self::ResidentAuthority
            | Self::ScriptGrant => return Vec::new(),
        };
        vec![
            DecisionAnswer::plain("approve", "Approve", approved),
            DecisionAnswer::plain("reject", "Reject", "Reject the task."),
        ]
    }

    /// The two answers of a [`Self::ResidentAuthority`] request, in
    /// presentation order. [`DecisionRequest::new`](super::model::DecisionRequest::new) marks both as
    /// authority-tier.
    pub fn resident_authority_answers() -> Vec<DecisionAnswer> {
        vec![
            DecisionAnswer::plain(
                RESIDENT_AUTHORITY_GRANT,
                "Grant",
                "Add exactly the requested rights and workspaces to the resident's authority.",
            ),
            DecisionAnswer::plain(
                RESIDENT_AUTHORITY_REJECT,
                "Reject",
                "Leave the resident's authority unchanged.",
            ),
        ]
    }

    /// The two answers of a [`Self::ScriptGrant`] request, in presentation
    /// order. [`DecisionRequest::new`](super::model::DecisionRequest::new) marks both as authority-tier.
    pub fn script_grant_answers() -> Vec<DecisionAnswer> {
        vec![
            DecisionAnswer::plain(
                SCRIPT_GRANT_GRANT,
                "Grant",
                "Grant the script at exactly this digest and capability set; its step runs on the next attempt.",
            ),
            DecisionAnswer::plain(
                SCRIPT_GRANT_REJECT,
                "Reject",
                "Grant nothing; the step keeps refusing until the script changes or an operator grants it.",
            ),
        ]
    }
}

/// Answer id that writes the grant a script grant request asks for.
pub const SCRIPT_GRANT_GRANT: &str = "grant";
/// Answer id that refuses a script grant request.
pub const SCRIPT_GRANT_REJECT: &str = "reject";

/// Answer id that applies a resident grant request.
pub const RESIDENT_AUTHORITY_GRANT: &str = "grant";
/// Answer id that refuses a resident grant request.
pub const RESIDENT_AUTHORITY_REJECT: &str = "reject";

/// Answer id that confirms a model answer.
pub const MODEL_ANSWER_CONFIRM: &str = "confirm";
/// Answer id that vetoes a model answer.
pub const MODEL_ANSWER_VETO: &str = "veto";
