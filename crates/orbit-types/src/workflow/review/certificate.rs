//! Review manifest, certificate and landing [ORB-11333].

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::super::automation::SourceRevision;
use super::{
    CommitIdentity, RetainedObligation, RetiredValidation, ReviewAssurance, ReviewBudget,
    ReviewFinding, ReviewReport, ReviewValidation, ReviewVerdict,
};

/// The pinned, immutable input handed to the reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewManifest {
    /// Passing external results and their artifact paths, re-read with their
    /// logs for this candidate tree. Attempt and commit changes do not expire
    /// these checks; a reviewer repair that changes the tree does.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub satisfied_external_evidence: BTreeMap<String, super::super::ReviewExternalEvidence>,
    /// Earlier report on this task, retained as advisory continuation context.
    /// A new attempt still needs a report naming its own attempt identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_report: Option<ReviewReport>,
    pub schema_version: u32,
    pub attempt_id: String,
    pub lineage_key: String,
    pub task_ids: Vec<String>,
    /// Task-meaning digests per task, plus the combined digest the
    /// certificate binds to.
    pub task_digests: BTreeMap<String, String>,
    /// Owner-captured checks that the report must establish as required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_validation_commands: Option<Vec<String>>,
    pub task_meaning_digest: String,
    pub repository: String,
    pub base: SourceRevision,
    pub candidate: SourceRevision,
    pub implementation_commits: Vec<CommitIdentity>,
    /// The implementer's persisted execution summaries, by task.
    pub implementer_summaries: BTreeMap<String, String>,
    pub reviewer_crew: String,
    pub contract_version: u32,
    pub policy_version: u32,
    pub budget: ReviewBudget,
    pub remaining: ReviewConsumption,
    pub issued_at: DateTime<Utc>,
}

/// The reviewer identity actually used for an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerIdentity {
    pub crew: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// The implementer's resolved model, when known, so a same-model review
    /// is reported as such rather than presented as model diversity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementer_model: Option<String>,
    pub same_model_as_implementer: bool,
}

/// Consumed or remaining reviewer runtime. Records written before
/// [ORB-13992] also carry a `reviewer_starts` count; reading ignores it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewConsumption {
    pub seconds: u64,
}

/// The settled gate result for one attempt. A passed certificate is the
/// only thing that can later exclude a delivery from redundant review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCertificate {
    pub schema_version: u32,
    pub attempt_id: String,
    pub lineage_key: String,
    pub task_ids: Vec<String>,
    pub task_meaning_digest: String,
    pub repository: String,
    pub base: SourceRevision,
    /// The implementation the reviewer examined, before any repair.
    pub reviewed_candidate: SourceRevision,
    /// The final candidate the verdict binds to, after repairs.
    pub final_candidate: SourceRevision,
    pub implementation_commits: Vec<CommitIdentity>,
    pub repair_commits: Vec<CommitIdentity>,
    pub verdict: ReviewVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assurance: Option<ReviewAssurance>,
    pub findings: Vec<ReviewFinding>,
    pub validation: Vec<ReviewValidation>,
    /// Owner-captured host checks this certificate must establish. `None`
    /// denotes a legacy certificate without an authoritative check snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_validation_commands: Option<Vec<String>>,
    /// Whether the records establish the final candidate: every required
    /// check passed, every other record is consistent with its role, and no
    /// obligation an earlier report revision recorded was dropped. Failed
    /// diagnostics stay failed and are not part of what this asserts.
    pub validation_complete: bool,
    /// Required-check records earlier revisions of the attempt's report made
    /// that the final report does not repeat verbatim. Absent on
    /// certificates issued before report revisions were retained.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retained_obligations: Vec<RetainedObligation>,
    /// Retained record ids the final report retired, with their reasons
    /// [ORB-14370]. Absent on certificates issued before record ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_validation: Vec<RetiredValidation>,
    /// The scope validation sources were judged against: every task selector
    /// plus a `file:` selector for every path the candidate changed from its
    /// base. Absent on certificates issued before scope-bound roles.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_scope: Vec<String>,
    pub reviewer: ReviewerIdentity,
    pub consumed: ReviewConsumption,
    pub budget: ReviewBudget,
    /// The reason the gate stopped, for non-pass verdicts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<String>,
    /// Selectors the gate appended because a repaired finding declared an
    /// out-of-scope repair path. Empty when the reviewer already widened
    /// through the task API or no such repair occurred. Absent on certificates
    /// issued before this field existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selectors_widened: Vec<String>,
    pub issued_at: DateTime<Utc>,
}

/// How the reviewed candidate became the landed commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingTransformation {
    /// The landed commit is the reviewed candidate.
    FastForward,
    /// One squash commit onto the reviewed base with the same tree.
    Squash,
    /// A merge commit whose result tree equals the reviewed candidate.
    MergeCommit,
    /// The candidate commits were replayed onto the same base tree.
    Rebase,
    /// The mapping could not be verified.
    Unknown,
}

/// The verified relation between a certificate and an actual landing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewLanding {
    pub attempt_id: String,
    pub repository: String,
    pub branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<String>,
    pub landed: SourceRevision,
    /// The base the landing was applied onto (first parent).
    pub base_at_landing: SourceRevision,
    pub transformation: LandingTransformation,
    /// Whether the certificate still covers the landed content.
    pub covered: bool,
    /// Why coverage did not carry, when it did not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// Why a previously issued gate no longer covers a candidate or landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewInvalidation {
    VerdictNotPassed,
    ValidationIncomplete,
    /// The certificate predates the captured owner check contract and must
    /// be replaced by a fresh review under a current delivery admission.
    ValidationContractMissing,
    TaskMeaningChanged,
    CandidateChanged,
    BaseChanged,
    ObjectsMissing,
    ExternalLandingRace,
    MappingUnknown,
}

impl ReviewInvalidation {
    /// Stable reason label.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewInvalidation::VerdictNotPassed => "verdict_not_passed",
            ReviewInvalidation::ValidationIncomplete => "validation_incomplete",
            ReviewInvalidation::ValidationContractMissing => "validation_contract_missing",
            ReviewInvalidation::TaskMeaningChanged => "task_meaning_changed",
            ReviewInvalidation::CandidateChanged => "candidate_changed",
            ReviewInvalidation::BaseChanged => "base_changed",
            ReviewInvalidation::ObjectsMissing => "objects_missing",
            ReviewInvalidation::ExternalLandingRace => "external_landing_race",
            ReviewInvalidation::MappingUnknown => "mapping_unknown",
        }
    }
}
