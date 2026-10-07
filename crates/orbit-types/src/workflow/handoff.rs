//! Exact delivery evidence and owner completion authority for distributed handoffs.
//! These records execute no merge; a before-PR review they carry is evidence
//! the owner verifies, never authority on its own.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{ReviewTiming, ReviewVerdict, automation::SourceRevision};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffArtifactRef {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum HandoffDelivery {
    PullRequest {
        number: u64,
    },
    LocalCandidate,
    /// A clean base with a digest-pinned clean-tree verifier checkpoint.
    /// Covers both a no-op implementation and work already satisfied on base.
    NoDiff {
        evidence: HandoffArtifactRef,
    },
    AlreadyLanded {
        covering_commit: String,
        evidence: HandoffArtifactRef,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffCandidate {
    pub repository: String,
    pub source_branch: String,
    pub base_branch: String,
    pub landing_branch: String,
    pub candidate: SourceRevision,
    pub base: SourceRevision,
    pub delivery: HandoffDelivery,
}

/// What the leaf's review settled. `not_required` keeps its original
/// spelling, so handoffs recorded before before-PR evidence existed still
/// read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffReviewDisposition {
    /// The claim's contract captured `review.before_pr = false`: no reviewed
    /// SHA, verdict or reviewer artifact exists, and none is invented.
    NotRequired,
    /// The leaf ran the before-PR gate the claim's contract captured
    /// [ORB-13895].
    BeforePr(Box<HandoffReviewEvidence>),
}

/// The leaf's before-PR review of the candidate it hands off. Every field is
/// a claim the owner checks against the certificate artifact it holds and
/// its own observation of the candidate; none is trusted on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffReviewEvidence {
    /// The review attempt the certificate was issued for.
    pub attempt_id: String,
    pub verdict: ReviewVerdict,
    /// The head the verdict binds to: the candidate after the reviewer's fix
    /// commit when it made one. It must be the handed-off candidate.
    pub reviewed_head_sha: String,
    /// The base the reviewer examined the candidate against.
    pub reviewed_base_sha: String,
    /// The reviewer's own fix commit (the candidate's last commit), when it
    /// fixed findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_commit: Option<String>,
    pub reviewer_crew: String,
    /// The run the reviewer was invoked from.
    pub reviewer_run_id: String,
    /// The settled [`ReviewCertificate`](super::ReviewCertificate) in an owner-accessible task artifact.
    pub certificate: HandoffArtifactRef,
    /// Further reviewer evidence (manifest, report) the owner holds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<HandoffArtifactRef>,
    /// [ORB-14478] The result and log of every `host_sandbox_test` the
    /// leaf's host ran for this verdict, which the owner re-reads and checks
    /// against the certificate and candidate tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub host_evidence: Vec<HandoffArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffReview {
    pub policy: ReviewTiming,
    pub disposition: HandoffReviewDisposition,
}

impl HandoffReview {
    /// The disposition of a claim admitted with `review.before_pr = false`.
    pub fn not_required() -> Self {
        Self {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
        }
    }

    /// The before-PR evidence this handoff carries, if any.
    pub fn before_pr(&self) -> Option<&HandoffReviewEvidence> {
        match &self.disposition {
            HandoffReviewDisposition::BeforePr(evidence) => Some(evidence.as_ref()),
            HandoffReviewDisposition::NotRequired => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskHandoff {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub machine_id: String,
    pub run_id: String,
    pub candidate: HandoffCandidate,
    pub review: HandoffReview,
    pub execution_summary: String,
    pub validation: Vec<HandoffArtifactRef>,
    /// Canonical relative additions outside the original module footprint.
    /// Recomputed from Git by the follower and independently by the owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footprint_widening: Vec<String>,
}

/// Captured command output in an owner-accessible task artifact, never an agent reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffValidationLog {
    pub schema_version: u32,
    pub workspace_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub machine_id: String,
    pub run_id: String,
    pub candidate: HandoffCandidate,
    pub tested_head: String,
    pub command: String,
    pub exit_code: i32,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedHandoff {
    pub handoff_id: String,
    pub handoff: TaskHandoff,
    /// Owner-required commands captured at acceptance, never chosen by the worker.
    pub required_commands: Vec<String>,
    pub accepted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoffAuthorizationSource {
    Operator,
    /// The owner's standing completion policy (`[workflow]
    /// distributed_completion = "done"`), recorded when the handoff was
    /// accepted and rechecked against the owner's configuration at landing.
    OwnerPolicy {
        reference: String,
    },
    /// Retained only so rows written before operation mode was removed still
    /// decode; completion under this source is always refused.
    Grant {
        grant_id: String,
    },
}

/// Immutable. Revocation is a separate durable record, not an overwrite of approval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffAuthorization {
    pub authorization_id: String,
    pub handoff_id: String,
    pub workspace_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub candidate: HandoffCandidate,
    pub approver: String,
    pub created_at: DateTime<Utc>,
    pub source: HandoffAuthorizationSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandoffRevocation {
    pub authorization_id: String,
    pub actor: String,
    pub reason: String,
    pub revoked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingStartState {
    Pending,
    Revoked,
    /// The owner consumer verified this handoff's merge evidence and completed it.
    Completed,
}

/// Durable outbox consumed by the owner landing job. A pending request survives
/// restart without a drain or ship sweep; only verified completion or explicit
/// revocation settles it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingStartRequest {
    pub handoff_id: String,
    pub authorization_id: String,
    pub state: LandingStartState,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingAttemptState {
    /// One owner landing job owns this handoff until it merges or stops.
    Dispatched,
    /// Merged and completed against verified external evidence.
    Merged,
    /// Stopped with durable evidence; a repair needs fresh validation and a new handoff.
    Stopped,
}

/// The owner's durable record of landing work for one handoff. Handoff identity
/// deduplicates job creation: a second dispatch for a live attempt is refused
/// rather than starting a second merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingAttempt {
    pub handoff_id: String,
    pub authorization_id: String,
    pub task_id: String,
    pub claim_id: String,
    pub attempt: u32,
    pub state: LandingAttemptState,
    /// The owner-local job run carrying this attempt, once it is submitted. An
    /// attempt reserved before submission records it on the next dispatch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Existing no-diff delivery contract shared with the local verifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyLandedEvidence {
    pub schema_version: u32,
    pub task_id: String,
    pub run_id: String,
    pub tested_head: String,
    pub covering_commit: String,
    pub covering_task_id: String,
    pub scope: serde_json::Value,
    pub required_commands: Vec<String>,
    pub validation: Vec<AlreadyLandedCheck>,
    pub criteria_evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlreadyLandedCheck {
    #[serde(flatten)]
    pub validation: super::ReviewValidation,
    pub log_artifact: String,
}

/// A run's claim that its implementation correctly changed nothing. Bound to
/// the task, the run, and the pinned HEAD it validated; unlike
/// [`AlreadyLandedEvidence`] it names no covering commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDiffEvidence {
    pub schema_version: u32,
    pub task_id: String,
    pub run_id: String,
    pub tested_head: String,
    pub reason: String,
    pub validation: Vec<NoDiffCheck>,
}

/// One validation command the no-diff run executed, with its exit status and
/// the task artifact holding the captured log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoDiffCheck {
    pub command: String,
    pub exit_code: i32,
    pub log_artifact: String,
}

pub fn already_landed_scope(
    task: &crate::task::Task,
    comments: &[crate::task::TaskComment],
) -> serde_json::Value {
    serde_json::json!({
        "title": task.title, "description": task.description,
        "acceptance_criteria": task.acceptance_criteria, "plan": task.plan,
        "context_files": task.context_files, "tags": task.tags, "relations": task.relations,
        "required_tools": task.required_tools, "type": task.task_type, "comments": comments,
    })
}
