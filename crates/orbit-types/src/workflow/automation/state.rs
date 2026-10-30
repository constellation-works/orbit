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
    /// Provider attempts for commits without a recorded association. Missing
    /// on older checkpoints; retained only while the commit is unresolved.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lookup_retries: BTreeMap<String, AssociationLookupRetry>,
    /// Oldest retained reservation. Legacy single-active checkpoints keep this
    /// exact attempt, including its original batch and action identities.
    pub active: Option<BatchAttempt>,
    /// Later disjoint reservations, in source order. Covered reservations stay
    /// here until every earlier range has accepted evidence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reservations: Vec<BatchAttempt>,
    /// Delivery review auto-tasks reserve independently; other consumers retain
    /// their single-active scheduling contract.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub independent_review: bool,
    /// Why this consumer stopped making progress. A recorded stall suspends
    /// evaluation until an audited recovery or reset clears it, so the reason
    /// is reported once as a durable fact instead of every tick as an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stall: Option<recovery::AutomationStall>,
}

/// Persisted retry progress for an unresolved provider lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssociationLookupRetry {
    /// Time of the last provider attempt, including an unavailable response.
    pub last_checked_at: chrono::DateTime<chrono::Utc>,
    /// Consecutive attempts without recording a provider association.
    pub attempts: u32,
}

/// Existing inspection surfaces render the same domain projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationDiagnostic {
    pub reason: String,
    /// Retained review batches without accepted coverage, independently of
    /// whether the scheduler can observe or reserve later ranges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub coverage_gaps: Vec<String>,
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

/// Hard reservation bound per independent delivery consumer. Accepted batches
/// behind a coverage hole count too, so a permanently unpaid gap bounds storage.
pub const MAX_DELIVERY_RESERVATIONS: usize = 64;

impl AutomationState {
    /// Every retained delivery attempt, in source order.
    pub fn delivery_attempts(&self) -> impl Iterator<Item = &BatchAttempt> {
        self.active.iter().chain(&self.reservations)
    }

    /// Find a retained attempt by immutable batch identity.
    pub fn delivery_attempt_mut(&mut self, batch_id: &str) -> Option<&mut BatchAttempt> {
        self.active
            .iter_mut()
            .chain(&mut self.reservations)
            .find(|attempt| attempt.batch.id == batch_id)
    }

    /// First unreserved commit offset in the retained observation window.
    pub fn reserved_commit_count(&self) -> usize {
        self.delivery_attempts()
            .map(|attempt| attempt.batch.commits.len())
            .sum()
    }

    /// Pending deliveries that have not already been frozen for an executor.
    pub fn unreserved_deliveries(&self) -> impl Iterator<Item = &Delivery> {
        let commits = &self.pending_commits[self.reserved_commit_count()..];
        self.pending
            .iter()
            .filter(move |delivery| delivery.commits.iter().all(|sha| commits.contains(sha)))
    }

    /// Retained independent review batches that still lack accepted evidence.
    pub fn delivery_coverage_gaps(&self) -> Vec<String> {
        if !self.independent_review {
            return vec![];
        }
        self.delivery_attempts()
            .filter(|attempt| attempt.state != super::BatchState::Covered)
            .map(|attempt| attempt.batch.id.clone())
            .collect()
    }

    /// Boundary after the last reservation, independently of accepted coverage.
    pub fn reserved_through(&self) -> &SourceRevision {
        self.reservations
            .last()
            .or(self.active.as_ref())
            .map_or(&self.covered, |attempt| &attempt.batch.through_inclusive)
    }
}
