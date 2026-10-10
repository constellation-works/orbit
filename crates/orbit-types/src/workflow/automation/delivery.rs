//! Delivery triggers, verified landings and source pages.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::DeliveryAssociation;

/// Examination contract stored on a delivery trigger, frozen batch, and
/// coverage evidence.
///
/// [`CoverageClass::LandedCodeReviewV1`] is the coverage a new definition may
/// select. [`CoverageClass::IntegratedQaV1`] is retired: serde still decodes
/// it so historical batches, evidence, automation state, and a not-yet-refreshed
/// workspace copy of the old delivery definition keep loading, and the
/// evaluator still applies that contract (exclusions are ignored). Auto-task
/// add, and an update that sets a schedule, refuse to select it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageClass {
    /// Retired QA coverage. Decode-only for persisted records.
    IntegratedQaV1,
    /// Review of landed deliveries. A passed before-PR certificate excludes
    /// a landing from the obligations.
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
    pub fn validate(&self) -> Result<(), super::super::error::WorkflowError> {
        if self.branch.is_empty()
            || self.branch.starts_with('-')
            || self.branch.chars().any(char::is_whitespace)
            || self.threshold == 0
            || self.threshold > self.max_items
            || self.max_items > 50
            || self.max_wait_minutes == 0
            || self.retries > 5
        {
            return Err(super::super::error::WorkflowError::Invalid(
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
    /// The tasks whose delivery this is, from the landing record.
    pub task_ids: Vec<String>,
    /// Why `task_ids` is empty, when the landing record names no task. A fact
    /// recorded before attribution carries neither, so its empty `task_ids`
    /// means unknown rather than none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unattributed: Option<String>,
    pub evidence_reference: String,
    pub evidence_digest: String,
    pub landed_at: DateTime<Utc>,
}

/// No task in this workspace records the landing: a PR merged outside Orbit,
/// or a direct landing whose run named no task.
pub const UNATTRIBUTED_NO_LANDING_TASK: &str = "no_landing_task";

/// This checkout cannot read the task records that would attribute the landing.
pub const UNATTRIBUTED_TASKS_UNREADABLE: &str = "task_records_unreadable";

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
    /// Retry progress from provider attempts in this observation.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub lookup_retries: BTreeMap<String, super::AssociationLookupRetry>,
    /// Accepted before-PR review coverage keyed by delivery key, supplied by
    /// Core from verified certificates [ORB-11333]. Only a
    /// `landed_code_review_v1` consumer excludes on it. A decoded
    /// `integrated_qa_v1` record ignores it.
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
    /// The assurance label the certificate carries, or `rebased_clean` when
    /// the landing carried the reviewed candidate cleanly onto a base that
    /// moved after review.
    pub assurance: String,
    /// The certificate's task-meaning digest, retained for audit.
    pub task_meaning_digest: String,
    /// The verified reviewed tree the landing reproduced, or carried onto
    /// the moved base for a `rebased_clean` exclusion.
    pub final_candidate_tree: String,
}

/// A delivery excluded from review obligations, with the reason it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedDelivery {
    pub delivery: Delivery,
    pub exclusion: DeliveryExclusion,
    pub decided_at: DateTime<Utc>,
}
