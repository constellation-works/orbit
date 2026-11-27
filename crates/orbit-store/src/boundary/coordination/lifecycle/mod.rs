//! Internal attempt lifecycle on the existing task commit journal. No public pull route.

pub(super) mod codec;
mod evidence;
mod inspect;
mod late_effort;
mod leaf_evidence;
mod mutate;
mod releases;
mod retention;
mod steering;

use crate::boundary::v2_bundle::TaskBundleV2;
use crate::contract::{ClaimInspection, ClaimMutationResult, ExecutionClaim, TaskCoordinationRow};
#[cfg(test)]
pub(super) use codec::STATE_DECODES;
pub(super) use codec::{CLAIM, STATE, decode, decode_state, encode, invalid, row};
use orbit_types::task::{ArtifactManifestV2, TaskCommentRowV2, TaskStatus};
pub(super) use releases::{DrainReleases, fresh_offer_history};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MutationReceipt {
    input: String,
    result: ClaimMutationResult,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct EvidenceIntent {
    summary: Option<String>,
    #[serde(default)]
    plan: Option<String>,
    comments_len: Option<u64>,
    comments: Vec<TaskCommentRowV2>,
    artifacts: Vec<orbit_types::task::TaskArtifact>,
    manifest: Option<ArtifactManifestV2>,
}

/// What [`super::TaskCommitBoundary::claim_authority`] established about a claim.
struct ClaimAuthority {
    row: TaskCoordinationRow,
    claim: ExecutionClaim,
    state: ClaimInspection,
    state_row: Option<TaskCoordinationRow>,
    bundle: TaskBundleV2,
    expected_status: TaskStatus,
}
