//! Durable owner notices, shared by polling and streaming lease clients.

use serde::{Deserialize, Serialize};

/// One immutable event. Sequence numbers increase across the owner's database,
/// hence within each workspace; gaps are normal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseNotice {
    /// Resume cursor, exclusive when supplied to a read.
    pub seq: u64,
    /// Only lease holders receive notices, never drain tasks.
    pub lease_id: String,
    /// Event kind, such as `granted` or `overlap_landed`.
    pub kind: String,
    /// Event-specific attribution and paths.
    pub payload: serde_json::Value,
}

/// Verified landed paths and their owner attribution.
pub struct LeaseLandedNoticeParams {
    /// Owner workspace identity.
    pub workspace_id: String,
    /// Legacy workspace location used by reservation scoping.
    pub workspace_orbit_dir: String,
    /// Full landed commit identifier.
    pub commit: String,
    /// Task whose delivery recorded the commit.
    pub task_id: String,
    /// Base ref containing this commit.
    pub base_branch: String,
    /// Landing time, in RFC 3339.
    pub landed_at: String,
    /// Changed path selectors, including both sides of renames.
    pub paths: Vec<String>,
}
