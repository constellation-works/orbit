//! Recovery, reset and stall contracts for a delivery consumer.
//!
//! A delivery consumer stops making progress for a few distinct reasons: its
//! definition was edited so the persisted epoch no longer matches configured
//! settings, a rebase orphaned its observed revision, or the evaluator hit a
//! source fact that will not change on its own. These types describe the
//! explicit, audited ways out — what the operator asks for, what the consumer
//! currently owes, and what was actually changed.
//!
//! Recovery never waives, covers or discards debt. Reset is the one operation
//! that forgets it, which is why its record carries the whole inventory of
//! what disappeared.

use super::{BatchState, DeliveryTrigger, SourceRevision};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What an operator explicitly authorizes for one stalled consumer. Settings
/// and action operations are opt-in; history replay previews until it carries
/// an explicit reason.
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
    /// Reconcile an orphaned observed history with the configured branch after
    /// a content-preserving rebase. Preview is inert; apply requires `reason`.
    #[serde(default)]
    pub replay_history: bool,
    /// Operator explanation, retained verbatim in the audit record.
    #[serde(default)]
    pub reason: String,
}

impl RecoveryRequest {
    /// True when the request asks for a durable change rather than a preview.
    pub fn mutates(&self) -> bool {
        self.adopt_settings
            || self.reissue_action
            || (self.replay_history && !self.reason.trim().is_empty())
    }
}

/// What an operator explicitly authorizes when resetting one consumer. Reset
/// forgets every retained obligation, so it previews until it carries a
/// reason.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetRequest {
    /// Operator explanation, retained verbatim in the audit record. Empty
    /// means preview only.
    #[serde(default)]
    pub reason: String,
    /// Reset even while an admitted action is still executing. The action is
    /// abandoned, not cancelled.
    #[serde(default)]
    pub force: bool,
}

impl ResetRequest {
    /// True when the request asks for a durable reset rather than a preview.
    pub fn mutates(&self) -> bool {
        !self.reason.trim().is_empty()
    }
}

/// What a reset forgot, and the baseline it left behind. Retained inside the
/// audit record so the discarded debt stays readable after the state is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetRecord {
    /// Generation the forgotten state carried.
    pub previous_generation: u64,
    /// Everything the consumer owed at the moment it was reset.
    pub forgotten: CoverageDebt,
    /// The frozen action abandoned by the reset, when one was held.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abandoned_action: Option<StalledAction>,
    /// Head of the configured branch the next evaluation re-baselines at.
    pub baseline: SourceRevision,
    /// Pinned `refs/orbit/automation/<consumer-digest>/*` refs released with
    /// the forgotten batches.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub released_refs: Vec<String>,
    /// The stall the reset cleared, when the consumer carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_stall: Option<AutomationStall>,
}

/// Preview or applied projection of a consumer reset. Both carry the same
/// document, so an operator verifies exactly what a reason would forget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetPreview {
    pub consumer: String,
    /// Current scheduling reason, in the shared inspection vocabulary.
    pub reason: String,
    pub generation: u64,
    pub epoch: String,
    /// Everything the reset would forget, or did.
    pub debt: CoverageDebt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<StalledAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<AutomationStall>,
    /// Head of the configured branch the consumer re-baselines at.
    pub baseline: SourceRevision,
    /// Named refusals blocking the reset; empty means it may run.
    pub refusals: Vec<String>,
    /// True only when this call durably reset the consumer.
    pub applied: bool,
    /// Recent audited recoveries for this consumer, newest first.
    pub history: Vec<RecoveryRecord>,
}

/// Why a consumer stopped making progress, recorded on its state so repeated
/// evaluations report one durable fact instead of a new error every tick.
///
/// Only a reason that needs an operator is recorded: evaluation stays
/// suspended while the marker is present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomationStall {
    /// The deferred reason, in the evaluator's own vocabulary.
    pub reason: String,
    /// First evaluation that saw this reason.
    pub since: DateTime<Utc>,
    /// When the stall was escalated to a warning and a friction record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalated_at: Option<DateTime<Utc>>,
    /// The friction record filed for this stall, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction_id: Option<String>,
    /// Divergence facts, present only for `history_diverged`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divergence: Option<HistoryDivergence>,
}

/// The unprovable history rewrite behind a `history_diverged` stall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryDivergence {
    /// The observed revision the rewrite orphaned.
    pub observed: SourceRevision,
    /// Head of the configured branch when the divergence was detected.
    pub head: SourceRevision,
    /// The refusal that stopped the automatic replay proof. Empty when the
    /// proof succeeded and the evaluator replayed the rewrite itself.
    pub refusal: String,
    /// Retained obligations the proof could not map onto the new head:
    /// delivery keys and unresolved commit ids.
    pub obligations: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed_history: Option<HistoryReplayRecord>,
    /// Present only on a reset: the debt this record forgot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<ResetRecord>,
    /// The friction record this recovery answers, when a stall filed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub friction_id: Option<String>,
    pub reason: String,
    pub by: String,
    pub at: DateTime<Utc>,
}

impl RecoveryRecord {
    /// Audit kind, in the operator-facing vocabulary.
    pub fn kind(&self) -> &'static str {
        if self.reset.is_some() {
            "reset"
        } else if self.replayed_history.is_some() {
            "replay_history"
        } else if self.reissued.is_some() && self.adopted_settings {
            "adopt_settings+reissue_action"
        } else if self.reissued.is_some() {
            "reissue_action"
        } else {
            "adopt_settings"
        }
    }
}

/// Actor recorded on a recovery the evaluator applied without an operator.
pub const SYSTEM_ACTOR: &str = "system:automation";

/// Default minutes a deferred reason may persist before the evaluator
/// escalates it to a warning and one friction record
/// (`automation.stall_window_minutes`).
pub const DEFAULT_STALL_WINDOW_MINUTES: u32 = 60;

/// One deterministic orphan-to-canonical commit correspondence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryMapping {
    pub orphan: SourceRevision,
    pub canonical: SourceRevision,
    /// Digest of the exact parent-relative binary patch plus the `.orbit` tree.
    pub proof_digest: String,
}

/// Audited facts for a history replay. The captured head and generation are
/// the compare-and-set fence; the coverage facts are intentionally repeated so
/// an audit reader can see that replay did not manufacture examination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryReplayRecord {
    pub captured_generation: u64,
    pub captured_head: SourceRevision,
    pub common_base: SourceRevision,
    pub old_observed: SourceRevision,
    pub new_observed: SourceRevision,
    pub mappings: Vec<HistoryMapping>,
    pub added_obligations: Vec<String>,
    pub unchanged_baseline: SourceRevision,
    pub unchanged_covered: SourceRevision,
    pub accepted_receipts: usize,
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
    /// Planned or applied history reconciliation, absent for ordinary recovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_replay: Option<HistoryReplayRecord>,
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
    /// Label recorded when an orphaned observation was replayed.
    pub const REPLAYED_HISTORY: &'static str = "replayed_history";
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
    /// History replay cannot be combined with configuration/action recovery.
    pub const RECOVERY_MODE_CONFLICT: &str = "recovery_mode_conflict";
    /// The observed revision is already ancestral to the configured head.
    pub const HISTORY_NOT_DIVERGED: &str = "history_not_diverged";
    /// A persisted orphan revision is no longer available from Git.
    pub const HISTORY_OBJECT_MISSING: &str = "history_object_missing";
    /// Content/topology proof found zero or multiple canonical counterparts.
    pub const HISTORY_MAPPING_AMBIGUOUS: &str = "history_mapping_ambiguous";
    /// Covered, baseline, or frozen batch boundaries do not reach the head.
    pub const HISTORY_BOUNDARY_UNREACHABLE: &str = "history_boundary_unreachable";
    /// The rebase exceeds the bounded first-parent proof window.
    pub const HISTORY_TRAVERSAL_LIMIT: &str = "history_traversal_limit";
    /// Inserted commits lack an unambiguous provider-owned delivery identity.
    pub const PROVIDER_PROOF_UNAVAILABLE: &str = "provider_proof_unavailable";
    /// Persisted logical identity differs from the canonical evidence.
    pub const HISTORY_CONTRACT_DRIFT: &str = "history_contract_drift";
    /// Reconciliation would remove an unpaid logical delivery.
    pub const HISTORY_DEBT_LOST: &str = "history_debt_lost";
    /// The configured branch moved after the preview facts were captured.
    pub const HISTORY_HEAD_CHANGED: &str = "history_head_changed";
    /// A reset was requested while an action is executing, without `--force`.
    pub const ACTION_EXECUTING: &str = "action_executing";
}
