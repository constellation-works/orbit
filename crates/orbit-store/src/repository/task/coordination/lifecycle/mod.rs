//! Internal attempt lifecycle on the existing task commit journal. No public pull route.

use orbit_types::task::{ArtifactManifestV2, TaskCommentRowV2, TaskStatus};
use serde::{Deserialize, Serialize};

use crate::contracts::{ClaimInspection, ClaimMutationResult, ExecutionClaim, TaskCoordinationRow};
use crate::repository::task::v2_bundle::TaskBundleV2;

mod codec;
mod evidence;
mod inspect;
mod mutate;
mod releases;

pub(super) use codec::{CLAIM, STATE, decode, encode, invalid, row};
pub(super) use releases::CandidateOffer;

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
