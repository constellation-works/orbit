//! Frozen coverage batches, attempts and waivers.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{CoverageClass, Delivery, ExcludedDelivery, SourceRevision};

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
