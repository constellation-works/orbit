//! A CI observation held until its owner's execution claim settles: a
//! task-branch failure, or a compiler or completed-repair receipt.
//!
//! The sweep must not write the owner's bundle while a claim protects it.
//! The commit boundary queues a receipt only while a claim protects its owner;
//! otherwise it retains the artifact and marks the row applied immediately.

use serde::{Deserialize, Serialize};

/// Coordination-row kind for one deferred CI observation.
pub const DEFERRED_BRANCH_OBSERVATION_KIND: &str = "ci-branch-observation-deferred-v1";

/// The retention decision made under the claim commit boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchObservationOutcome {
    /// The receipt is retained on its owner.
    Retained,
    /// A current protecting claim holds the receipt until settlement.
    Deferred { claiming_run_id: String },
}

/// One CI receipt waiting to land on its owner.
///
/// `content` is the artifact body `file_ci_failure_tasks` would have written.
/// Retention or claim settlement writes it at `artifact_path`, with `comment`
/// when the owner lacks it, and sets `applied`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredBranchObservation {
    pub schema_version: u32,
    pub task_id: String,
    /// CI run id from the failure row, not the claiming Orbit run.
    pub run_id: serde_json::Value,
    pub job_id: serde_json::Value,
    pub artifact_path: String,
    pub content: String,
    /// Orbit run that held the claim when the sweep deferred the write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claiming_run_id: Option<String>,
    /// System comment to append to the owner with the receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applied: bool,
}
