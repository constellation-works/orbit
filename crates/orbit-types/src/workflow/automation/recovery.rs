//! Operator recovery contracts for a stalled delivery consumer [ORB-12295].
//!
//! A delivery consumer stalls when its definition is edited: the persisted
//! epoch no longer matches the configured one, so nothing is admitted and the
//! retained obligations sit still. These types describe the explicit, audited
//! way out — what the operator asks for, what the consumer currently owes, and
//! what was actually changed. Nothing here waives, covers or discards debt.

use super::{BatchState, DeliveryTrigger, SourceRevision};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What an operator explicitly authorizes for one stalled consumer. Both
/// operations are opt-in: an empty request previews and changes nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryRequest {
    /// Adopt the definition's current compatible settings, retaining every
    /// covered, pending, unresolved and accepted fact the consumer holds.
    #[serde(default)]
    pub adopt_settings: bool,
    /// Reissue the settled action that closed without accepted evidence, over
    /// its existing frozen obligations.
    #[serde(default)]
    pub reissue_action: bool,
    /// Operator explanation, retained verbatim in the audit record.
    #[serde(default)]
    pub reason: String,
}

impl RecoveryRequest {
    /// True when the request asks for a durable change rather than a preview.
    pub fn mutates(&self) -> bool {
        self.adopt_settings || self.reissue_action
    }
}

/// The configuration identity a consumer carries, against the configured one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryIdentity {
    /// Epoch the persisted consumer was admitted under.
    pub recorded_epoch: String,
    /// Epoch the definition resolves to now.
    pub configured_epoch: String,
    /// The resolved trigger the consumer recorded, absent for a consumer
    /// baselined before recovery existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_trigger: Option<DeliveryTrigger>,
    pub configured_trigger: DeliveryTrigger,
    /// Named compatible differences, such as `threshold` or `template`.
    pub changes: Vec<String>,
}

/// Everything the consumer still owes, preserved across any recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageDebt {
    pub baseline: SourceRevision,
    pub covered: SourceRevision,
    pub observed: SourceRevision,
    pub pending_deliveries: usize,
    pub pending_commits: usize,
    pub unresolved: usize,
    pub waived: usize,
    pub excluded: usize,
    pub receipts: usize,
}

/// The frozen action a stalled consumer is holding, and whether this recovery
/// may reissue it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StalledAction {
    pub batch_id: String,
    pub attempt: u32,
    pub state: BatchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Exact delivery keys frozen into the batch; a reissue carries these over
    /// unchanged.
    pub obligations: Vec<String>,
    pub commits: usize,
    pub reissuable: bool,
}

/// One applied recovery, retained as immutable audit. It records the replaced
/// identity, so the pre-recovery configuration stays readable afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryRecord {
    pub consumer: String,
    pub previous_epoch: String,
    pub epoch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_trigger: Option<DeliveryTrigger>,
    /// The trigger the consumer carries after this recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<DeliveryTrigger>,
    pub adopted_settings: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reissued: Option<ReissuedAction>,
    pub reason: String,
    pub by: String,
    pub at: DateTime<Utc>,
}

/// The settled action a recovery reissued, and the attempt it authorized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReissuedAction {
    pub batch_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_action_id: Option<String>,
    pub from_attempt: u32,
    pub from_state: BatchState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_reason: Option<String>,
    pub attempt: u32,
    pub authorization: super::ActionReissue,
}

/// Read-only projection of a consumer's recovery position. Preview and apply
/// return the same document, so an operator verifies exactly what changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPreview {
    pub consumer: String,
    /// Current scheduling reason, in the shared inspection vocabulary.
    pub reason: String,
    pub identity: RecoveryIdentity,
    pub debt: CoverageDebt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<StalledAction>,
    /// Named refusals blocking the requested recovery; empty means it may run.
    pub refusals: Vec<String>,
    /// What this call durably changed. Always empty for a preview.
    pub applied: Vec<String>,
    /// Recent audited recoveries for this consumer, newest first.
    pub history: Vec<RecoveryRecord>,
}

impl RecoveryPreview {
    /// Label recorded in `applied` when settings were adopted.
    pub const ADOPTED_SETTINGS: &'static str = "adopted_settings";
    /// Label recorded in `applied` when the settled action was reissued.
    pub const REISSUED_ACTION: &'static str = "reissued_action";
}

/// Why a recovery cannot run. Each variant names one deterministic refusal so
/// operators and tests share the vocabulary.
pub mod refusal {
    /// This host holds no persisted state for the definition.
    pub const UNKNOWN_CONSUMER: &str = "unknown_consumer";
    /// The consumer is a state-member consumer, not a delivery consumer.
    pub const MEMBER_CONSUMER: &str = "member_consumer";
    /// An admitted or claimed action is still executing.
    pub const ACTIVE_EXECUTION: &str = "active_execution";
    /// The configured branch differs from the branch the debt was observed on.
    pub const BRANCH_CHANGED: &str = "branch_changed";
    /// The source repository identity differs from the recorded one.
    pub const REPOSITORY_CHANGED: &str = "repository_changed";
    /// The resolved owner machine differs from the recorded one.
    pub const OWNER_CHANGED: &str = "owner_changed";
    /// The examination contract differs; QA and review never share coverage.
    pub const COVERAGE_CHANGED: &str = "coverage_changed";
    /// Nothing proves which examination contract the retained debt was for.
    pub const COVERAGE_UNVERIFIABLE: &str = "coverage_unverifiable";
    /// This host may not admit work for the definition.
    pub const OWNED_ELSEWHERE: &str = "owned_elsewhere";
    /// Adoption was requested but the configured identity already matches.
    pub const SETTINGS_UNCHANGED: &str = "settings_unchanged";
    /// A reissue was requested with no settled unevidenced action to reissue.
    pub const NO_SETTLED_ACTION: &str = "no_settled_action";
    /// The action already produced accepted coverage evidence.
    pub const ACTION_EVIDENCED: &str = "action_evidenced";
    /// A reissue would never admit while the recorded identity is stale.
    pub const DEFINITION_CHANGED: &str = "definition_changed";
    /// A durable recovery requires a non-empty operator explanation and actor.
    pub const MISSING_AUTHORIZATION: &str = "missing_authorization";
}
