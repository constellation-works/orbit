//! Review landing, release, and invocation requests.

use orbit_types::workflow::ReviewerInvocationEvent;

/// What completion observed about a reviewed candidate's managed landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLandingRequest {
    pub run_id: String,
    pub task_ids: Vec<String>,
    pub workspace_path: std::path::PathBuf,
    pub pr_number: String,
    pub base: String,
    pub reviewed_head_sha: String,
    /// A conditional synchronous merge returned the same commit later observed.
    pub managed_merge: bool,
    /// The merge commit the provider reported, when it reported one.
    pub landed_commit: Option<String>,
}

/// A review attempt whose reviewer step failed or whose run is ending
/// without a settled verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReleaseRequest {
    /// The run closing the attempt; its own reviewer runtime is charged.
    pub run_id: String,
    pub lineage_key: String,
    pub attempt_id: String,
}

/// A before-PR reviewer invocation starting or ending for its attempt, so
/// the lineage is charged reviewer process runtime only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewerInvocationRequest {
    /// The run executing the reviewer step.
    pub run_id: String,
    pub lineage_key: String,
    pub attempt_id: String,
    pub event: ReviewerInvocationEvent,
}

/// A reviewer that returned, asking about the report it persisted before
/// settlement judges it: whether it holds a defect the reviewer can still
/// correct [ORB-14616], or, for a reviewer that declared failure, whether it
/// is an incomplete verdict whose only gap is named external evidence
/// [ORB-25667].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReportCorrectionRequest {
    /// The run executing the reviewer step.
    pub run_id: String,
    pub lineage_key: String,
    pub attempt_id: String,
    /// Every task of the reviewed bundle.
    pub task_ids: Vec<String>,
    /// The reviewed worktree.
    pub workspace_path: std::path::PathBuf,
}
