//! Coverage evidence, accepted receipts and evidence artifacts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{BatchAttempt, CoverageClass, SourceRevision};

/// Current evidence schema. V3 adds explicit completed examinations and
/// immutable progress references. V2 final evidence still settles legacy
/// batches; v1 still decodes for historical receipts but cannot settle anew.
pub const COVERAGE_EVIDENCE_SCHEMA_VERSION: u32 = 3;

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
    /// One record per frozen delivery: the changed paths read, the verdict
    /// and why. Absent from version-1 evidence.
    #[serde(default)]
    pub delivery_examinations: Vec<DeliveryExamination>,
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

/// What a reviewer examined in one frozen delivery and what it concluded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryExamination {
    /// The frozen delivery's key.
    pub delivery: String,
    /// Explicit completion; absent/false entries are provisional. Only v3
    /// evidence can contribute durable partial progress.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub completed: bool,
    /// Digest of the immutable, server-verified earlier evidence containing
    /// this exact examination. Never changes that artifact's action/attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_digest: Option<String>,
    /// Paths of the delivery's `before..after` diff the reviewer read.
    pub examined_paths: Vec<String>,
    /// Paths of that diff deliberately not read, each with its reason.
    #[serde(default)]
    pub skipped_paths: Vec<SkippedPath>,
    pub verdict: DeliveryVerdict,
    /// Why the verdict holds for this delivery's change.
    pub rationale: String,
}

/// A changed path a reviewer did not read, such as a generated golden.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkippedPath {
    pub path: String,
    pub reason: String,
}

/// A reviewer's conclusion about one delivery: `"clean"`, or
/// `{"findings": ["<finding task id>", ...]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryVerdict {
    Clean,
    Findings(Vec<String>),
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
    /// Set when the owner committed a claimed worker's bytes: the claim it
    /// authenticated and the follower machine that ran `run_id`. Absent from
    /// a witness the owner's own executor run wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<ClaimedSubmission>,
}

/// The owner's claim journal entry a claimed submission was authenticated
/// against, at the moment its bytes were committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimedSubmission {
    pub claim_id: String,
    /// The follower machine the claim was admitted to, which ran `run_id`.
    pub machine_id: String,
}

/// Shape supplied with a frozen batch. Workers fill each delivery's
/// examination, the checks and findings, and attest completion.
pub fn evidence_template(attempt: &BatchAttempt) -> CoverageEvidence {
    let mut template = CoverageEvidence {
        schema_version: COVERAGE_EVIDENCE_SCHEMA_VERSION,
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
        delivery_examinations: attempt
            .batch
            .deliveries
            .iter()
            .map(|d| DeliveryExamination {
                delivery: d.key.clone(),
                completed: false,
                source_digest: None,
                examined_paths: vec![],
                skipped_paths: vec![],
                verdict: DeliveryVerdict::Clean,
                rationale: String::new(),
            })
            .collect(),
        checks: vec![],
        findings: vec![],
    };
    // Verified state supplies immutable provenance. The runtime validates it
    // again before admission; template construction never grants authority.
    for progress in &attempt.progress {
        if let Ok(prior) = serde_json::from_slice::<CoverageEvidence>(&progress.evidence) {
            for mut examination in prior.delivery_examinations {
                if progress.deliveries.contains(&examination.delivery) {
                    examination.source_digest = Some(progress.evidence_digest.clone());
                    if let Some(entry) = template
                        .delivery_examinations
                        .iter_mut()
                        .find(|entry| entry.delivery == examination.delivery)
                    {
                        *entry = examination;
                    }
                }
            }
            for finding in prior.findings {
                if !template.findings.contains(&finding) {
                    template.findings.push(finding);
                }
            }
            for check in prior.checks {
                if !template.checks.contains(&check) {
                    template.checks.push(check);
                }
            }
        }
    }
    template
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

/// Server-captured immutable bytes of authorized completed examination.
/// Stored through the existing generation-fenced consumer checkpoint, never
/// accepted from a worker's evidence as a new grant of authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompletedReviewProgress {
    pub action_id: String,
    pub attempt: u32,
    pub evidence_digest: String,
    pub evidence: Vec<u8>,
    pub reference: String,
    pub submitted_by: String,
    /// Original Store-authored task witness; job-only evidence uses its
    /// persisted canonical step result instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<EvidenceSubmission>,
    pub captured_at: DateTime<Utc>,
    pub deliveries: Vec<String>,
}
