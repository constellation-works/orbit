//! Named external checks that hold a review without rejecting its candidate.

use serde::{Deserialize, Serialize};

use super::ValidationOutcome;
use super::automation::SourceRevision;

/// Durable hold attached by review settlement, never an acceptance certificate.
pub const REVIEW_EVIDENCE_HOLD_ARTIFACT: &str = "review-evidence-hold.json";

/// External environments whose evidence can arrive after the reviewer stops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvidenceKind {
    HostedCi,
    NativeOs,
    #[serde(rename = "codeql")]
    CodeQl,
}

/// One named external check and the task artifact where its result must arrive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvidenceRequirement {
    pub kind: ReviewEvidenceKind,
    pub name: String,
    pub command: String,
    pub artifact: String,
}

/// A hold pins the reviewed tree and task meaning; evidence never grants a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvidenceHold {
    pub schema_version: u32,
    pub attempt_id: String,
    pub lineage_key: String,
    pub run_id: String,
    pub candidate: SourceRevision,
    pub task_meaning_digest: String,
    pub requirements: Vec<ReviewEvidenceRequirement>,
}

/// Evidence attached at a requirement's artifact path, with a separately
/// attached log. A matching passing result releases the hold for fresh review.
/// Attempt, commit and display name record provenance; identity is kind,
/// command and tree, so another review of the same tree can reuse the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewExternalEvidence {
    pub schema_version: u32,
    pub attempt_id: String,
    pub candidate: SourceRevision,
    pub kind: ReviewEvidenceKind,
    pub name: String,
    pub command: String,
    pub outcome: ValidationOutcome,
    pub log_artifact: String,
}

impl ReviewExternalEvidence {
    /// Whether this result names the check on the candidate tree.
    /// The host must also validate the schema, outcome and attached log.
    pub fn matches_requirement(
        &self,
        requirement: &ReviewEvidenceRequirement,
        candidate: &SourceRevision,
    ) -> bool {
        !candidate.tree.is_empty()
            && self.candidate.tree == candidate.tree
            && self.kind == requirement.kind
            && self.command == requirement.command
    }
}
