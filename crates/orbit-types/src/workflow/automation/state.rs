//! Scheduler state and its diagnostic projection.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{
    BatchAttempt, BatchWaiver, CoverageReceiptSummary, Delivery, DeliveryAssociation,
    DeliveryOwnership, DeliveryTrigger, ExcludedDelivery, SourceRevision, members, recovery,
};

/// Small current scheduler state; completed batches/receipts are separate rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<members::MemberState>,
    pub consumer: String,
    pub epoch: String,
    /// The resolved trigger this consumer's epoch was derived from. Baselining
    /// records it and only an audited recovery replaces it, so the settings the
    /// retained debt was accumulated under stay provable. Absent on consumers
    /// baselined before it was recorded, and on state-member consumers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<DeliveryTrigger>,
    pub repository: String,
    pub branch: String,
    pub generation: u64,
    pub baseline: SourceRevision,
    pub observed: SourceRevision,
    pub covered: SourceRevision,
    pub pending_commits: Vec<String>,
    pub pending: Vec<Delivery>,
    #[serde(default)]
    pub waived: Vec<Delivery>,
    /// Deliveries proven covered before landing. They never count toward a
    /// threshold. An exclusively excluded prefix may retire without a consumer
    /// examination receipt; interleaved exclusions retire with the examined
    /// range that contains them.
    #[serde(default)]
    pub excluded: Vec<ExcludedDelivery>,
    pub unresolved: BTreeMap<String, String>,
    #[serde(default)]
    pub associations: BTreeMap<String, Option<DeliveryAssociation>>,
    pub active: Option<BatchAttempt>,
    /// Why this consumer stopped making progress. A recorded stall suspends
    /// evaluation until an audited recovery or reset clears it, so the reason
    /// is reported once as a durable fact instead of every tick as an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<recovery::AutomationStall>,
}

/// Existing inspection surfaces render the same domain projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationDiagnostic {
    pub reason: String,
    pub state: Option<AutomationState>,
    pub receipts: Vec<CoverageReceiptSummary>,
    pub waivers: Vec<BatchWaiver>,
    /// Resolved ownership for a delivery consumer. Absent on state-trigger
    /// routines, whose trigger always names its owner outright.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ownership: Option<DeliveryOwnership>,
    /// Members in the batch a state consumer would admit, is admitting, or
    /// has in flight, each with why it is there [ORB-12746]. Empty for
    /// delivery consumers and when nothing is due.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch: Vec<members::BatchMember>,
    /// Why an edited delivery definition was not adopted automatically, in
    /// the `recover` refusal vocabulary. Present only with
    /// `definition_changed`, and only where the evaluator would otherwise
    /// adopt the edit itself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refusals: Vec<String>,
}
