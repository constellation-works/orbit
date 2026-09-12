//! Shared delivery-trigger, batch and coverage contracts [ORB-11330].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod members;
pub mod recovery;

/// Supported examination contracts; QA and review never share acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageClass {
    IntegratedQaV1,
    LandedCodeReviewV1,
}

impl std::fmt::Display for CoverageClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let wire_name = match self {
            Self::IntegratedQaV1 => "integrated_qa_v1",
            Self::LandedCodeReviewV1 => "landed_code_review_v1",
        };

        formatter.write_str(wire_name)
    }
}

/// Opt-in delivery scheduling configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryTrigger {
    /// Stable registry machine ID. When omitted, the registered owner of the
    /// workspace owns the definition; execution stays inert while that owner
    /// is missing or contradicted.
    #[serde(default)]
    pub owner_machine: Option<String>,
    pub branch: String,
    pub threshold: usize,
    pub max_wait_minutes: u32,
    pub coverage: CoverageClass,
    #[serde(default = "default_batch_size")]
    pub max_items: usize,
    #[serde(default)]
    pub retries: u32,
}

fn default_batch_size() -> usize {
    50
}

impl DeliveryTrigger {
    pub fn validate(&self) -> Result<(), super::error::WorkflowError> {
        if self.branch.is_empty()
            || self.branch.starts_with('-')
            || self.branch.chars().any(char::is_whitespace)
            || self.threshold == 0
            || self.threshold > self.max_items
            || self.max_items > 50
            || self.max_wait_minutes == 0
            || self.retries > 5
        {
            return Err(super::error::WorkflowError::Invalid(
                "delivery trigger requires a branch, 1 <= threshold <= max_items <= 50, positive max_wait_minutes and retries <= 5".into(),
            ));
        }

        Ok(())
    }
}

/// A commit and its resulting tree, verified by the source adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRevision {
    pub commit: String,
    pub tree: String,
}

/// Canonical verified landing; task/commit membership does not multiply count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delivery {
    pub key: String,
    pub repository: String,
    pub branch: String,
    pub before: SourceRevision,
    pub after: SourceRevision,
    pub commits: Vec<String>,
    pub task_ids: Vec<String>,
    pub evidence_reference: String,
    pub evidence_digest: String,
    pub landed_at: DateTime<Utc>,
}

/// Bounded, pinned source observation. Unresolved commits remain obligations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourcePage {
    pub from: SourceRevision,
    pub through: SourceRevision,
    pub commits: Vec<String>,
    pub deliveries: Vec<Delivery>,
    pub unresolved: BTreeMap<String, String>,
    #[serde(default)]
    pub associations: BTreeMap<String, Option<DeliveryAssociation>>,
    /// Accepted before-PR review coverage keyed by delivery key, supplied by
    /// Core from verified certificates [ORB-11333]. Only a
    /// `landed_code_review_v1` consumer excludes on it; QA never does.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub exclusions: BTreeMap<String, DeliveryExclusion>,
    pub complete: bool,
}

/// A delivery whose content is proven covered by an accepted before-PR
/// review certificate. It stays in the examined range as context but is not
/// an obligation and does not count toward a review threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryExclusion {
    /// The certificate attempt that covers this delivery.
    pub attempt_id: String,
    /// The assurance label the certificate carries.
    pub assurance: String,
    /// The certificate's task-meaning digest, retained for audit.
    pub task_meaning_digest: String,
    /// The verified reviewed tree the landing reproduced.
    pub final_candidate_tree: String,
}

/// A delivery excluded from review obligations, with the reason it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedDelivery {
    pub delivery: Delivery,
    pub exclusion: DeliveryExclusion,
    pub decided_at: DateTime<Utc>,
}

/// Frozen input supplied verbatim to the task or job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageBatch {
    pub schema_version: u32,
    pub id: String,
    pub consumer: String,
    pub epoch: String,
    pub repository: String,
    pub branch: String,
    pub coverage: CoverageClass,
    pub from_exclusive: SourceRevision,
    pub through_inclusive: SourceRevision,
    pub commits: Vec<String>,
    pub deliveries: Vec<Delivery>,
    /// Deliveries inside the range that accepted before-PR coverage excludes
    /// from examination; they are readable context, not obligations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<ExcludedDelivery>,
    pub created_at: DateTime<Utc>,
    /// Aggregate budget is frozen with the batch, including configuration edits.
    pub max_attempts: u32,
    pub retry_until: DateTime<Utc>,
}

/// Scheduling debt and successful examination are distinct durable states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchState {
    Claimed,
    Admitted,
    Failed,
    Exhausted,
    Waived,
    Covered,
}

/// One current attempt over immutable input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchAttempt {
    pub batch: CoverageBatch,
    pub input_digest: String,
    pub attempt: u32,
    pub action_key: String,
    pub action_id: Option<String>,
    pub state: BatchState,
    pub reason: Option<String>,
    pub retry_after: Option<DateTime<Utc>>,
    /// Operator authorization for the current attempt, present only when a
    /// recovery reissued a settled action over this same frozen batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reissue: Option<ActionReissue>,
}

impl BatchAttempt {
    /// Latest moment this attempt may still reach admission. The frozen batch
    /// budget governs, unless an operator explicitly authorized a reissue.
    pub fn deadline(&self) -> DateTime<Utc> {
        self.reissue
            .as_ref()
            .map_or(self.batch.retry_until, |reissue| reissue.retry_until)
    }
}

/// Recorded authorization for one additional attempt over an already frozen
/// batch, after the previous action settled without accepted evidence. It
/// grants exactly one attempt and never touches the batch or its obligations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionReissue {
    /// The settled action this attempt replaces, when one was admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_action_id: Option<String>,
    pub reason: String,
    pub by: String,
    pub at: DateTime<Utc>,
    /// Authorized admission deadline for this attempt alone.
    pub retry_until: DateTime<Utc>,
}

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
}

/// Worker-submitted structured evidence, attached through orbit.task.artifact.put.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageEvidence {
    pub schema_version: u32,
    pub batch_id: String,
    pub consumer: String,
    pub epoch: String,
    pub input_digest: String,
    pub action_id: String,
    pub attempt: u32,
    pub coverage: CoverageClass,
    pub from_exclusive: SourceRevision,
    pub through_inclusive: SourceRevision,
    pub examined_commits: Vec<String>,
    pub examined_deliveries: Vec<String>,
    pub examination_complete: bool,
    /// Concrete commands/checks and their observations. Findings may remain open.
    pub checks: Vec<ExaminationCheck>,
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExaminationCheck {
    pub subject: String,
    pub method: String,
    pub observation: String,
}

/// Immutable accepted bytes and provenance, independent of artifact replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedCoverage {
    pub batch_id: String,
    pub action_id: String,
    pub input_digest: String,
    pub evidence_digest: String,
    pub evidence: Vec<u8>,
    pub evidence_reference: String,
    pub submitted_by: String,
    pub accepted_at: DateTime<Utc>,
}

/// How the effective owner of a delivery consumer was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnerAuthority {
    /// The definition names `owner_machine` explicitly.
    Definition,
    /// Inherited from the registered owner of this workspace, which is
    /// authoritative whenever the definition omits an owner.
    Workspace,
    /// Nothing names an owner: the workspace record predates host identity,
    /// or this checkout is not registered.
    Missing,
    /// The workspace record and this checkout's replica role name different
    /// owners, so neither may be trusted.
    Conflicting,
}

/// Effective ownership of one delivery consumer on this host. Preview,
/// inspection and real evaluation all report it, so "no admission here" is
/// never indistinguishable from a definition the operator disabled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryOwnership {
    /// The machine allowed to admit work, when one could be resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_machine: Option<String>,
    pub authority: OwnerAuthority,
    /// True only when this host is the resolved owner. Admission is
    /// impossible otherwise, whatever the definition's `enabled` says.
    pub owned_here: bool,
}

impl DeliveryOwnership {
    /// Scheduling reason for an enabled definition this host may not admit
    /// work for; `None` when this host is the owner.
    pub fn refusal(&self) -> Option<&'static str> {
        if self.owned_here {
            return None;
        }

        Some(match self.authority {
            OwnerAuthority::Definition | OwnerAuthority::Workspace => "owned_elsewhere",
            OwnerAuthority::Missing | OwnerAuthority::Conflicting => "ownership_unresolved",
        })
    }
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
}

/// Core-verified writer authority for exact artifact bytes; this is not coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSubmission {
    pub action_id: String,
    pub evidence_digest: String,
    pub run_id: String,
}

/// Shape supplied with a frozen batch. Workers fill checks/findings and attest completion.
pub fn evidence_template(attempt: &BatchAttempt) -> CoverageEvidence {
    CoverageEvidence {
        schema_version: 1,
        batch_id: attempt.batch.id.clone(),
        consumer: attempt.batch.consumer.clone(),
        epoch: attempt.batch.epoch.clone(),
        input_digest: attempt.input_digest.clone(),
        action_id: attempt
            .action_id
            .clone()
            .unwrap_or_else(|| "<this-task-or-run-id>".into()),
        attempt: attempt.attempt,
        coverage: attempt.batch.coverage,
        from_exclusive: attempt.batch.from_exclusive.clone(),
        through_inclusive: attempt.batch.through_inclusive.clone(),
        examined_commits: attempt.batch.commits.clone(),
        examined_deliveries: attempt
            .batch
            .deliveries
            .iter()
            .map(|d| d.key.clone())
            .collect(),
        examination_complete: false,
        checks: vec![],
        findings: vec![],
    }
}

/// Reserved Store-authored artifact; callers cannot supply its contents.
pub const EVIDENCE_AUTHORITY_ARTIFACT: &str = "automation-evidence-authority.json";

/// Deterministic delivery-owner facts captured before attempting a direct landing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectLandingRequest {
    pub run_id: String,
    pub branch: String,
    pub before_commit: String,
    pub after_commit: String,
}

/// Provider association retained while a complete PR landing span is unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryAssociation {
    pub key: String,
    pub anchor: String,
    pub reference: String,
    pub landed_at: DateTime<Utc>,
}

/// Small diagnostic receipt; accepted bytes are downloaded separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReceiptSummary {
    pub batch_id: String,
    pub action_id: String,
    pub input_digest: String,
    pub evidence_digest: String,
    pub evidence_reference: String,
    pub submitted_by: String,
    pub accepted_at: DateTime<Utc>,
}

impl From<AcceptedCoverage> for CoverageReceiptSummary {
    fn from(receipt: AcceptedCoverage) -> Self {
        Self {
            batch_id: receipt.batch_id,
            action_id: receipt.action_id,
            input_digest: receipt.input_digest,
            evidence_digest: receipt.evidence_digest,
            evidence_reference: receipt.evidence_reference,
            submitted_by: receipt.submitted_by,
            accepted_at: receipt.accepted_at,
        }
    }
}

/// Explicit debt disposition. It never advances the covered revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchWaiver {
    pub batch_id: String,
    pub reason: String,
    pub by: String,
    pub at: DateTime<Utc>,
}

/// Existing definition-update surface accepts this administrative request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaiveBatchRequest {
    pub batch_id: String,
    pub reason: String,
}
