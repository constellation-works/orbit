//! What a claimed leaf's own run record said about its job, build and failing
//! step, kept beside the claim's settlement [ORB-20906].
//!
//! The leaf ran on a follower, so the owner's run history never holds it, and
//! [`super::ClaimSettlementRecord`] keeps only the settlement's own words. This
//! is the owner's account of the leaf's run record. It lives in its own
//! immutable coordination row, never in the lifecycle state a released client
//! rewrites, so neither an older lifecycle writer nor a later one can drop it.

use orbit_types::workflow::OrbitBuild;
use serde::{Deserialize, Serialize};

/// Largest failing-step message a [`LeafRootCause`] keeps.
pub const MAX_LEAF_ROOT_CAUSE_MESSAGE_BYTES: usize = 1024;

/// The failing step a leaf's run recorded: its own typed fields, never text
/// guessed from a diagnostic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafRootCause {
    /// The step the run record names as where it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// That step's recorded error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// That step's recorded error message, bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl LeafRootCause {
    /// Whether the run recorded none of the fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.step.is_none() && self.code.is_none() && self.message.is_none()
    }

    /// This cause with every field trimmed, empty ones dropped and the
    /// message cut to its bound.
    #[must_use]
    pub fn bounded(self) -> Self {
        let named = |value: Option<String>| {
            value
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        Self {
            step: named(self.step),
            code: named(self.code),
            message: named(self.message).map(|message| {
                let cut = orbit_common::text::floor_char_boundary(
                    &message,
                    MAX_LEAF_ROOT_CAUSE_MESSAGE_BYTES,
                );
                message[..cut].to_string()
            }),
        }
    }
}

/// The leaf run's recorded identity as its executing host read it from the
/// run record when the leaf settled, or as an operator's observation of that
/// host read it later. A field the run never recorded is absent: `build`
/// `None` means the run record held no build, not that one is unknown here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafRunReport {
    /// The leaf run, on its executing host's store.
    pub run_id: String,
    /// The leaf's job.
    pub job_id: String,
    /// The build recorded on the run, when it recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<OrbitBuild>,
    /// The failing step the run recorded, when it recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_cause: Option<LeafRootCause>,
}

/// Where a [`LeafRunEvidence`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeafEvidenceProvenance {
    /// The leaf's follower captured it before it recorded its first
    /// settlement and sent it with that settlement.
    Settlement,
    /// An operator observed the follower's run record after the settlement,
    /// through the follower's authenticated operator route.
    Hydrated,
}

impl LeafEvidenceProvenance {
    /// The provenance's wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Settlement => "settlement",
            Self::Hydrated => "hydrated",
        }
    }
}

/// A claim's leaf-run evidence as its owner recorded it: the report, the
/// claim tuple the owner bound it to, and where it came from. Immutable once
/// recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeafRunEvidence {
    pub provenance: LeafEvidenceProvenance,
    /// When the owner recorded it.
    pub recorded_at: String,
    pub claim_id: String,
    pub task_id: String,
    /// The machine the claim executed on.
    pub machine_id: String,
    /// The follower drain that admitted the claim.
    pub drain_run_id: String,
    pub report: LeafRunReport,
}

impl LeafRunEvidence {
    /// Whether `other` records the same fact, whenever and however it was
    /// recorded. An identical replay is accepted; anything else is a conflict.
    #[must_use]
    pub fn same_fact(&self, other: &Self) -> bool {
        self.claim_id == other.claim_id
            && self.task_id == other.task_id
            && self.machine_id == other.machine_id
            && self.drain_run_id == other.drain_run_id
            && self.report == other.report
    }
}

/// What recording hydrated evidence for a claim did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeafEvidenceWrite {
    /// New evidence was recorded.
    Recorded(Box<LeafRunEvidence>),
    /// The claim already holds the same fact; the first record stands.
    Replayed(Box<LeafRunEvidence>),
    /// The claim already holds different evidence. Nothing was written, and
    /// the held evidence stands.
    Conflict(Box<LeafRunEvidence>),
}
