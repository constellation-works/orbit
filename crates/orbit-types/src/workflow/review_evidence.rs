//! Named external checks that hold a review without rejecting its candidate.

use serde::{Deserialize, Serialize};

use super::ValidationOutcome;
use super::automation::SourceRevision;

/// Durable hold attached by review settlement, never an acceptance certificate.
pub const REVIEW_EVIDENCE_HOLD_ARTIFACT: &str = "review-evidence-hold.json";

/// The task history event that queues a held task for fresh review once every
/// named check arrived. Its note begins `run=<hold run>;`.
pub const REVIEW_EVIDENCE_RECEIVED_EVENT: &str = "review_evidence_received";

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
    /// [ORB-14450] The task's spec digest when the hold was written, so the
    /// next run resumes the held candidate only while the task's description
    /// and acceptance criteria are unchanged. Absent on older holds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_spec_digest: Option<String>,
}

/// [ORB-14450] Evidence on an earlier candidate tree counted for this one:
/// the candidate was rebased onto a new base with its patch unchanged
/// (`git patch-id --stable` of the whole base..head diff).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvidenceCarried {
    pub from_tree: String,
    pub to_tree: String,
    pub patch_id: String,
}

/// Why evidence on an earlier candidate tree does not count for this one, so
/// the named checks are requested again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEvidenceRerequestReason {
    /// The candidate's patch over its base differs from the one the
    /// evidence was checked on: a conflict resolution or another change.
    PatchChanged,
    /// The earlier candidate or its base is not readable here, so the
    /// patches cannot be compared.
    SourceUnavailable,
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
