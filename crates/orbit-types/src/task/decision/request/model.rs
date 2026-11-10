//! Persisted request and resolution data and request builders.

use serde::{Deserialize, Serialize};

use crate::task::{TaskHistoryEntry, TaskStatus};
use crate::workflow::ResidentGrantRequest;
use crate::workflow::activity_job::ScriptGrantRequest;

use super::binding::DecisionBinding;
use super::gates::{DecisionGate, MODEL_ANSWER_CONFIRM, MODEL_ANSWER_VETO};
use super::options::{DecisionAction, DecisionAnswer, DecisionRiskTier};

/// History event that raises a [`DecisionRequest`].
pub const DECISION_REQUESTED_EVENT: &str = "decision_requested";
/// History event that records an applied answer to a [`DecisionRequest`].
pub const DECISION_ANSWERED_EVENT: &str = "decision_answered";
/// History event that re-opens a precedent answer. A later human answer
/// clears it. Restoring status without this event does not reopen a request.
pub const DECISION_REOPENED_EVENT: &str = "decision_reopened";
/// Writer label of an answer applied from an operator-approved corpus rule.
/// It is not a human answer and the human apply path refuses it.
pub const PRECEDENT_ACTOR: &str = "precedent";
/// Writer label of an answer the decision model applied. It is an automation
/// actor ([`crate::identity::is_automation_actor`]), so no guard that needs a
/// human resolution accepts it, and the human apply path refuses it.
/// [ORB-22808] Decision assistance was removed: nothing writes model answers
/// any more, and the model-answer records stay so earlier history still reads.
pub const MODEL_ANSWER_ACTOR: &str = "model";
/// History event a human writes when confirming a model answer through its
/// [`DecisionGate::ModelAnswerReview`] request. The note is a
/// [`ModelAnswerConfirmation`]; only a confirmed model answer is precedent.
pub const DECISION_MODEL_CONFIRMED_EVENT: &str = "decision_model_confirmed";
/// Status-note marker on the bookkeeping status moves a precedent answer
/// and its reversal write. Those moves must not permanently supersede the
/// request: the reversal puts the task back and the request reads open again.
pub const PRECEDENT_BOOKKEEPING_NOTE: &str = "precedent-bookkeeping";
/// History event that records a decision token consumed by a refused
/// submission that had already authenticated (design
/// `docs/design/human-decision-channel/2_design.md` §2.3). The note names
/// the request and the token's SHA-256 fingerprint, never the token.
pub const DECISION_TOKEN_CONSUMED_EVENT: &str = "decision_token_consumed";
/// Default lifetime of a decision request, in hours.
pub const DEFAULT_DECISION_REQUEST_TTL_HOURS: i64 = 72;
/// How close to expiry an open request must be for the decision digest
/// to return it as nearing expiry, in hours.
pub const DECISION_DIGEST_EXPIRY_WINDOW_HOURS: i64 = 24;

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

/// One human answer a precedent rule cites.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrecedentSource {
    /// Task whose human answer the rule cites.
    pub task: String,
    /// Calendar date of that answer (`YYYY-MM-DD`).
    pub date: String,
    /// Option id that answer chose.
    pub option: String,
}

/// Audit fields that separate a precedent answer from a human one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrecedentCitation {
    /// Corpus rule that supplied the option.
    pub rule_id: String,
    /// Human answers the rule was curated from.
    pub citations: Vec<PrecedentSource>,
    /// The operator set `auto` on the rule. Consolidation never does.
    pub operator_approved: bool,
    /// Task status to restore when the operator reverses the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<TaskStatus>,
}

/// One precedent a model answer cites, as the precedent provider supplied it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCitedPrecedent {
    /// Provider that supplied the precedent.
    pub provider: String,
    /// Provider-stable precedent id.
    pub precedent_id: String,
    /// When the cited human decision was made.
    pub decided_at: chrono::DateTime<chrono::Utc>,
    /// Citation source name.
    pub source: String,
    /// Stable reference inside the source.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Content digest of the cited record.
    pub digest: String,
}

/// The category enablement a model answer ran under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelAnswerEnablement {
    /// Operator who enabled the category.
    pub operator: String,
    /// The readout the enablement cites (`<task>/<artifact>`).
    pub readout: String,
}

/// Audit fields that separate a model answer from a human one. Recorded
/// only when `answered_by` is [`MODEL_ANSWER_ACTOR`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelAnswerCitation {
    /// Assessment that produced the answer.
    pub assessment_id: String,
    /// Logical id of that assessment.
    pub logical_id: String,
    /// Category the request belongs to.
    pub category: String,
    /// Assistance policy version.
    pub policy_version: String,
    /// Assistance policy digest.
    pub policy_digest: String,
    /// Question-set version.
    pub question_set: String,
    /// Assistance contract version.
    pub contract_version: String,
    /// Adapter that served the assessment.
    pub adapter: String,
    /// Model identity requested.
    pub requested_model: String,
    /// Model identity the service returned.
    pub returned_model: String,
    /// Model revision epoch.
    pub model_revision: String,
    /// Probability of the chosen answer.
    pub probability: f64,
    /// The operating threshold the probability met.
    pub threshold: f64,
    /// Probability that the supplied precedents conflict.
    pub conflict_probability: f64,
    /// Precedents the answer cites.
    pub cited_precedents: Vec<ModelCitedPrecedent>,
    /// The enablement the answer ran under.
    pub enablement: ModelAnswerEnablement,
    /// Task status to restore when the answer is vetoed or reversed. The
    /// apply step records it; any value the caller supplies is replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_status: Option<TaskStatus>,
}

// Probabilities are checked to lie in [0, 1] before an answer is applied, so
// no stored citation holds a NaN and equality is total.
impl Eq for ModelAnswerCitation {}

/// Note body of a [`DECISION_MODEL_CONFIRMED_EVENT`]: the human who
/// confirmed the model answer `answered_at` on `request_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelAnswerConfirmation {
    /// The model-answered request.
    pub request_id: String,
    /// `answered_at` of the confirmed model resolution.
    pub answered_at: chrono::DateTime<chrono::Utc>,
    /// The confirming human; the event's writer.
    pub confirmed_by: String,
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
    /// Set only when `answered_by` is [`PRECEDENT_ACTOR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precedent: Option<PrecedentCitation>,
    /// Set only when `answered_by` is [`MODEL_ANSWER_ACTOR`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelAnswerCitation>,
    /// The human confirmation of a model answer, derived on read from a
    /// later [`DECISION_MODEL_CONFIRMED_EVENT`]; never written with the
    /// answer itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<ModelAnswerConfirmation>,
}

impl DecisionResolution {
    /// Whether an automation writer (the `precedent` rule actor, the
    /// `model` actor or any other automation label) applied this answer.
    pub fn is_automation_answer(&self) -> bool {
        let actor = self.answered_by.trim();
        actor == PRECEDENT_ACTOR || crate::identity::is_automation_actor(actor)
    }
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
    /// Most severe answer risk, or unknown for a legacy request.
    #[serde(default)]
    pub risk_tier: DecisionRiskTier,
    /// Category fixed when raised; absent on legacy requests.
    #[serde(default)]
    pub category: Option<String>,
    /// One or two plain-language sentences naming the decision needed,
    /// readable without opening the code. Absent on older requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Id of the answer the raiser recommends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    /// The exact action outside Orbit that unblocks the task, when no option
    /// can: the request then carries one "Done — requeue" answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_action: Option<String>,
    /// Structured topic (or corpus rule id) an agent-raised request carries,
    /// so a later reader matches it by exact value, never by wording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// Writer label of the agent that raised an agent-raised request; the
    /// per-filer cap counts open requests by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filer: Option<String>,
    /// Stale-guard binding the request was raised against.
    pub binding: DecisionBinding,
    /// Mechanism that raised it (`block_triage`, `task_pilot`, `digest`,
    /// `agent_request`).
    pub raised_by: String,
    /// When it was raised.
    pub raised_at: chrono::DateTime<chrono::Utc>,
    /// `raised_at + request_ttl_hours`; the token expires with it.
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Stored state; effective state is derived by
    /// [`decision_requests_from_history`](super::history::decision_requests_from_history).
    pub state: DecisionRequestState,
    /// Recorded when the request is answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<DecisionResolution>,
    /// The exact addition a [`DecisionGate::ResidentAuthority`] request asks
    /// for, with the relayed quote verbatim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resident_grant: Option<ResidentGrantRequest>,
    /// The script, digest, capability set and diff a
    /// [`DecisionGate::ScriptGrant`] request asks consent for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_grant: Option<ScriptGrantRequest>,
}

impl DecisionRequest {
    /// A new open request with a generated id, expiring after the
    /// default TTL ([`DEFAULT_DECISION_REQUEST_TTL_HOURS`]).
    pub fn new(
        task_id: String,
        gate: DecisionGate,
        mut answers: Vec<DecisionAnswer>,
        binding: DecisionBinding,
        raised_by: String,
        raised_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let id = Self::new_id(&task_id, &binding, raised_at);
        for answer in &mut answers {
            answer.risk_tier = match gate {
                DecisionGate::ReviewApproval => DecisionRiskTier::Irreversible,
                DecisionGate::ProposedApproval | DecisionGate::PilotAdmission => {
                    DecisionRiskTier::Authority
                }
                DecisionGate::BlockTriage => match answer
                    .option
                    .as_ref()
                    .and_then(|option| option.action.as_ref())
                {
                    Some(DecisionAction::Reject | DecisionAction::Land) => {
                        DecisionRiskTier::Irreversible
                    }
                    Some(_) => DecisionRiskTier::Reversible,
                    None if answer.id == "dismiss" => DecisionRiskTier::Routine,
                    None if answer
                        .option
                        .as_ref()
                        .is_some_and(|option| option.rewrite.is_some()) =>
                    {
                        DecisionRiskTier::Reversible
                    }
                    None => DecisionRiskTier::Unknown,
                },
                DecisionGate::AgentRequest => answer.risk_tier,
                DecisionGate::ModelAnswerReview => match answer.id.as_str() {
                    MODEL_ANSWER_CONFIRM => DecisionRiskTier::Routine,
                    MODEL_ANSWER_VETO => DecisionRiskTier::Reversible,
                    _ => DecisionRiskTier::Unknown,
                },
                // Either answer is the human's call on authority.
                DecisionGate::ResidentAuthority | DecisionGate::ScriptGrant => {
                    DecisionRiskTier::Authority
                }
            };
        }
        let risk_tier = answers
            .iter()
            .map(|answer| answer.risk_tier)
            .max()
            .unwrap_or_default();
        Self {
            id,
            task_id,
            gate,
            answers,
            risk_tier,
            category: None,
            summary: None,
            recommended: None,
            human_action: None,
            topic: None,
            filer: None,
            binding,
            raised_by,
            raised_at,
            expires_at: raised_at + chrono::Duration::hours(DEFAULT_DECISION_REQUEST_TTL_HOURS),
            state: DecisionRequestState::Open,
            resolution: None,
            resident_grant: None,
            script_grant: None,
        }
    }

    /// Attach the script, digest, capabilities and diff a script grant
    /// request asks consent for.
    pub fn with_script_grant(mut self, grant: ScriptGrantRequest) -> Self {
        self.script_grant = Some(grant);
        self
    }

    /// Attach the exact addition a resident grant request asks for.
    pub fn with_resident_grant(mut self, grant: ResidentGrantRequest) -> Self {
        self.resident_grant = Some(grant);
        self
    }

    /// Fix the category at raise time for this workspace.
    pub fn with_category(mut self, workspace: &str) -> Self {
        let kind = if self.gate == DecisionGate::AgentRequest {
            let file = self
                .topic
                .as_deref()
                .map(|topic| topic.split('/').next().unwrap_or(topic))
                .unwrap_or("untopiced");
            format!("agent_request/{file}")
        } else {
            self.gate.as_str().to_string()
        };
        self.category = Some(format!("{workspace}/{kind}"));
        self
    }

    /// This request with the short summary and the recommended answer id.
    pub fn with_summary(mut self, summary: String, recommended: Option<String>) -> Self {
        self.summary = Some(summary);
        self.recommended = recommended;
        self
    }

    /// This request carrying the structured `topic` and the `filer` that raised it.
    pub fn with_topic_and_filer(mut self, topic: Option<String>, filer: String) -> Self {
        self.topic = topic;
        self.filer = Some(filer);
        self
    }

    /// This request carrying the exact action only a human outside Orbit can take.
    pub fn with_human_action(mut self, human_action: String) -> Self {
        self.human_action = Some(human_action);
        self
    }

    /// The answer `answer_id` names, if this request allows it.
    pub fn answer(&self, answer_id: &str) -> Option<&DecisionAnswer> {
        self.answers.iter().find(|answer| answer.id == answer_id)
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
