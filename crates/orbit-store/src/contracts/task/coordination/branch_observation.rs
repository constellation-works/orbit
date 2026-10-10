//! A task-branch CI observation held until the owner's execution claim settles.
//!
//! The sweep must not write the owner's bundle while a claim protects it.
//! The row is append-only until settlement marks it applied in the same commit
//! that retains the artifact.

use serde::{Deserialize, Serialize};

/// Coordination-row kind for one deferred task-branch CI observation.
pub const DEFERRED_BRANCH_OBSERVATION_KIND: &str = "ci-branch-observation-deferred-v1";

/// One branch-failure receipt waiting to land on its owner.
///
/// `content` is the artifact body `file_ci_failure_tasks` would have written.
/// Settlement writes it at `artifact_path` and sets `applied`.
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub applied: bool,
}
