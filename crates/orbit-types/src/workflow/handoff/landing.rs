//! Landing records of a handoff: attempts, stop evidence, event names and
//! operator retry requests.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingStartState {
    Pending,
    Revoked,
    /// The owner consumer verified this handoff's merge evidence and completed it.
    Completed,
}

/// Durable outbox consumed by the owner landing job. A pending request survives
/// restart without a drain; only verified completion or explicit revocation
/// settles it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingStartRequest {
    pub handoff_id: String,
    pub authorization_id: String,
    pub state: LandingStartState,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingAttemptState {
    /// One owner landing job owns this handoff until it merges or stops.
    Dispatched,
    /// Merged and completed against verified external evidence.
    Merged,
    /// Stopped with durable evidence. The owner may automatically retry a cleared
    /// waiting gate within its budget; an operator may explicitly retry a healthy
    /// authorized handoff. Candidate repair needs validation and a new handoff.
    Stopped,
}

/// The owner's durable record of landing work for one handoff. Handoff identity
/// deduplicates job creation: a second dispatch for a live attempt is refused
/// rather than starting a second merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingAttempt {
    pub handoff_id: String,
    pub authorization_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub attempt: u32,
    pub state: LandingAttemptState,
    /// The owner-local job run carrying this attempt, once it is submitted. An
    /// attempt reserved before submission records it on the next dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Discriminator of an [`OwnerLandingRefusal`]: its `kind` is always
/// `owner_landing_refusal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerLandingRefusalKind {
    OwnerLandingRefusal,
}

/// The bounded, redacted human reason of an [`OwnerLandingRefusal`], with
/// the size of the text it was cut from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingRefusalReason {
    pub value: String,
    #[serde(default)]
    pub original_chars: usize,
    #[serde(default)]
    pub truncated: bool,
}

/// A waiting gate that can clear without changing an accepted candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingWaitGate {
    /// The base's checks must finish successfully before landing resumes.
    BaseChecksPending,
    /// Required or owner-owed checks are still running on the candidate.
    RequiredChecksPending,
    /// Failed-job logs must become readable before the failure can be classified.
    JobLogsUnavailable,
}

impl LandingWaitGate {
    /// The delivery stop code recorded with an automatic retry reservation.
    pub fn stop_code(self) -> &'static str {
        match self {
            Self::BaseChecksPending => "delivery_base_checks_pending",
            Self::RequiredChecksPending => "delivery_requirements_unsatisfied",
            Self::JobLogsUnavailable => "delivery_forge_observation_unavailable",
        }
    }
}

/// The diagnostic an owner landing stop records as its attempt's evidence.
///
/// Fields serialize in this declaration order, which is the stored byte
/// order: keep it. The bounded observations no reader interprets stay
/// opaque JSON; they default to `null` when older evidence omits them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OwnerLandingRefusal {
    #[serde(default)]
    pub schema_version: u32,
    pub kind: OwnerLandingRefusalKind,
    pub reason: LandingRefusalReason,
    #[serde(default)]
    pub repairable: bool,
    /// Typed retry eligibility, separate from opaque diagnostic observations.
    /// Older stops lacking this evidence remain available for explicit retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_gate: Option<LandingWaitGate>,
    #[serde(default)]
    pub accepted: serde_json::Value,
    #[serde(default)]
    pub owner_owed: serde_json::Value,
    #[serde(default)]
    pub observation: serde_json::Value,
    #[serde(default)]
    pub workflow_prerequisites: serde_json::Value,
    #[serde(default)]
    pub emission_proof: serde_json::Value,
    /// The engine sets this to `true` for every `StopForRequeue`. If the
    /// landing repair budget is spent, the task is blocked while this evidence
    /// stays on the attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requeue: Option<bool>,
}

/// [`LandingAttempt::evidence`] read once: the stored string stays opaque
/// and byte-identical, and every reader decodes it through
/// [`LandingStopEvidence::parse`].
#[derive(Debug, Clone, PartialEq)]
pub enum LandingStopEvidence {
    /// An `owner_landing_refusal` diagnostic with a non-blank reason.
    OwnerRefusal(Box<OwnerLandingRefusal>),
    /// A stop reason the engine recorded as a JSON string literal: `raw`
    /// is the stored text, quotes included, and `decoded` its content.
    JsonString { raw: String, decoded: String },
    /// Anything else, shown as stored: older plain text, merge evidence,
    /// other JSON, or a refusal whose reason is missing or blank.
    Text(String),
}

impl LandingStopEvidence {
    /// Decode one stored evidence string. Never fails: what is not a
    /// readable refusal or JSON string stays [`LandingStopEvidence::Text`].
    pub fn parse(evidence: &str) -> Self {
        let raw = || evidence.to_string();
        match serde_json::from_str::<serde_json::Value>(evidence) {
            Err(_) => Self::Text(raw()),
            Ok(serde_json::Value::String(decoded)) => Self::JsonString {
                raw: raw(),
                decoded,
            },
            Ok(value) => match serde_json::from_value::<OwnerLandingRefusal>(value) {
                Ok(refusal) if !refusal.reason.value.trim().is_empty() => {
                    Self::OwnerRefusal(Box::new(refusal))
                }
                _ => Self::Text(raw()),
            },
        }
    }

    /// The text status notes, revocation reasons, repair stop text and
    /// blocked comments carry: a refusal's human reason, otherwise the
    /// evidence exactly as stored.
    pub fn operator_reason(&self) -> &str {
        match self {
            Self::OwnerRefusal(refusal) => &refusal.reason.value,
            Self::JsonString { raw, .. } | Self::Text(raw) => raw,
        }
    }
}

/// [ORB-16890] Times a task returns to the backlog because its handed-off
/// candidate fails its own required check at landing, after its one
/// automatic repair leaf, before the next such stop blocks it.
pub const LANDING_REPAIR_BUDGET: usize = 2;

/// History event of a landing stop that ended the claim and returned the
/// task to the backlog, keeping its candidate for the next run's repair.
pub const LANDING_REPAIR_REQUEUED_EVENT: &str = "landing_repair_requeued";

/// Status event recorded when Orbit settles a task to `done` from its
/// delivery pull request merged outside Orbit. Like the delivery pipeline's
/// own completion, the settlement is not an edge of the status table: the
/// forge's merge, not a caller's claim, is its evidence. The owner's claim
/// journal records it too, when it settles a revoked handoff's claim in the
/// same commit.
pub const EXTERNAL_MERGE_SETTLED_EVENT: &str = "external_merge_settled";

/// History event of an owner landing that waits for its base branch to
/// recover from a failure inherited from it, before the landing deadline.
pub const LANDING_WAITING_FOR_BASE_EVENT: &str = "landing_waiting_for_base";

/// History event of the review watch that follows an owner-local delivery
/// left in review with an open pull request (a dashboard ship, `orbit run
/// ship` or a drain leaf, none of which hands off for landing). Its note is
/// the JSON outcome, a [`RecordedLandingChecksObservation`](super::RecordedLandingChecksObservation): `state`
/// (`passed`, `failed`, `conflicting`, `blocked` or `pending`), `pr_number`,
/// `head_sha`, and for `failed` the landing's `classification` of the failed
/// checks with its `detail`.
pub const LANDING_CHECKS_OBSERVED_EVENT: &str = "landing_checks_observed";

/// History event of the landing stop that found the task's landing repair
/// budget spent and blocked it. Requeues before it no longer count.
pub const LANDING_REPAIR_EXHAUSTED_EVENT: &str = "landing_repair_exhausted";

/// The landing repair requeues that count against a task's budget, from its
/// history event names in order: those after its last exhaustion, which an
/// operator answered by unblocking the task.
pub fn landing_repairs_spent<'a>(events: impl IntoIterator<Item = &'a str>) -> usize {
    events.into_iter().fold(0, |spent, event| match event {
        LANDING_REPAIR_REQUEUED_EVENT => spent + 1,
        LANDING_REPAIR_EXHAUSTED_EVENT => 0,
        _ => spent,
    })
}

/// An operator's compare-and-reserve request for the same immutable handoff.
/// No pipeline input or replacement authority is accepted by this operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandingRetryRequest {
    pub handoff_id: String,
    pub expected_candidate_commit: String,
    pub expected_base_commit: String,
    pub expected_latest_attempt: u32,
    pub expected_latest_state: LandingAttemptState,
    pub reason: String,
    pub request_key: String,
}

/// Durable outbox receipt, committed with the reserved attempt before submission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingRetryReceipt {
    pub request: LandingRetryRequest,
    pub actor: String,
    pub attempt: u32,
    pub created_at: DateTime<Utc>,
}
