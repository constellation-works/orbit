//! Negotiated, digest-pinned evidence for owner verification of live claims.

use serde::{Deserialize, Serialize};

use crate::workflow::handoff::HandoffArtifactRef;

/// The supported claimed visual-resolution bundle schema. A missing capability
/// never authorizes live claimed delivery.
pub const CLAIMED_VISUAL_RESOLUTION_SCHEMA: u32 = 1;
/// System-written bundle returned in a live claim's `TaskHandoff::visual`.
pub const VISUAL_RESOLUTION_ARTIFACT: &str = "visual/resolution.json";

/// Evidence references, rather than a worker's assertion of success. The owner
/// independently checks the frozen contract, complete final capture, reviewer's
/// report and its own dispute history on acceptance and before landing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimedVisualResolution {
    pub schema_version: u32,
    pub task_id: String,
    pub claim_id: String,
    pub run_id: String,
    pub candidate_sha: String,
    /// Digest of the exact serialized visual contract in the admission receipt.
    pub contract_sha256: String,
    pub initial_capture: HandoffArtifactRef,
    pub final_capture: HandoffArtifactRef,
    pub final_checks: HandoffArtifactRef,
    pub review: HandoffArtifactRef,
}
