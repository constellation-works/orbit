//! Coverage evidence, accepted receipts and evidence artifacts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{BatchAttempt, CoverageClass, SourceRevision};

/// Worker-submitted structured evidence, attached through orbit.task.artifact.put.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageEvidence {
    pub schema_version: u32,
    pub batch_id: String,
    pub consumer: String,
    pub epoch: String,
    pub input_digest: String,
    pub action_id: String,
    pub attempt: u32,
    pub coverage: CoverageClass,
    pub from_exclusive: SourceRevision,
    pub through_inclusive: SourceRevision,
    pub examined_commits: Vec<String>,
    pub examined_deliveries: Vec<String>,
    pub examination_complete: bool,
    /// Concrete commands/checks and their observations. Findings may remain open.
    pub checks: Vec<ExaminationCheck>,
    pub findings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExaminationCheck {
    pub subject: String,
    pub method: String,
    pub observation: String,
}

/// Immutable accepted bytes and provenance, independent of artifact replacement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedCoverage {
    pub batch_id: String,
    pub action_id: String,
    pub input_digest: String,
    pub evidence_digest: String,
    pub evidence: Vec<u8>,
    pub evidence_reference: String,
    pub submitted_by: String,
    pub accepted_at: DateTime<Utc>,
}

/// Core-verified writer authority for exact artifact bytes; this is not coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSubmission {
    pub action_id: String,
    pub evidence_digest: String,
    pub run_id: String,
}

/// Shape supplied with a frozen batch. Workers fill checks/findings and attest completion.
pub fn evidence_template(attempt: &BatchAttempt) -> CoverageEvidence {
    CoverageEvidence {
        schema_version: 1,
        batch_id: attempt.batch.id.clone(),
        consumer: attempt.batch.consumer.clone(),
        epoch: attempt.batch.epoch.clone(),
        input_digest: attempt.input_digest.clone(),
        action_id: attempt
            .action_id
            .clone()
            .unwrap_or_else(|| "<this-task-or-run-id>".into()),
        attempt: attempt.attempt,
        coverage: attempt.batch.coverage,
        from_exclusive: attempt.batch.from_exclusive.clone(),
        through_inclusive: attempt.batch.through_inclusive.clone(),
        examined_commits: attempt.batch.commits.clone(),
        examined_deliveries: attempt
            .batch
            .deliveries
            .iter()
            .map(|d| d.key.clone())
            .collect(),
        examination_complete: false,
        checks: vec![],
        findings: vec![],
    }
}

/// Task artifact carrying an action's [`CoverageEvidence`]. The artifact Store
/// refuses bytes that do not parse as that schema, so the submitter sees the
/// parse error while it can still fix and re-put the file.
pub const COVERAGE_ARTIFACT: &str = "automation-coverage.json";

/// Reserved Store-authored artifact; callers cannot supply its contents.
pub const EVIDENCE_AUTHORITY_ARTIFACT: &str = "automation-evidence-authority.json";

/// Small diagnostic receipt; accepted bytes are downloaded separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReceiptSummary {
    pub batch_id: String,
    pub action_id: String,
    pub input_digest: String,
    pub evidence_digest: String,
    pub evidence_reference: String,
    pub submitted_by: String,
    pub accepted_at: DateTime<Utc>,
}

impl From<AcceptedCoverage> for CoverageReceiptSummary {
    fn from(receipt: AcceptedCoverage) -> Self {
        Self {
            batch_id: receipt.batch_id,
            action_id: receipt.action_id,
            input_digest: receipt.input_digest,
            evidence_digest: receipt.evidence_digest,
            evidence_reference: receipt.evidence_reference,
            submitted_by: receipt.submitted_by,
            accepted_at: receipt.accepted_at,
        }
    }
}
