//! Delivery evidence of a handoff: the candidate, its review, the task handoff
//! with its validation records, and the owner's completion authorization.
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::workflow::{ReviewTiming, ReviewVerdict, automation::SourceRevision};

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
    /// The claim's contract captured no review before landing: no reviewed
    /// SHA, verdict or reviewer artifact exists, and none is invented.
    NotRequired,
    /// The leaf ran the before-PR gate the claim's contract captured
    /// [ORB-13895].
    BeforePr(Box<HandoffReviewEvidence>),
    /// The leaf reviewed its open pull request before handing it off, as
    /// the claim's contract captured `review.timing = before_landing` [ORB-14849].
    BeforeLanding(Box<HandoffReviewEvidence>),
}

/// The leaf's review of the candidate it hands off. Every field is
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
    /// The settled [`ReviewCertificate`](crate::workflow::ReviewCertificate) in an owner-accessible task artifact.
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
    /// [ORB-22808] The selective-selection receipt an earlier leaf sent.
    /// Selective review was removed: this build never sends one and the
    /// owner ignores it. Kept until 2026-12-08 because this type denies
    /// unknown fields, so such a handoff still parses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selective_receipt: Option<serde_json::Value>,
}

impl HandoffReview {
    /// The disposition of a claim admitted with no review before landing.
    pub fn not_required() -> Self {
        Self {
            policy: ReviewTiming::None,
            disposition: HandoffReviewDisposition::NotRequired,
            selective_receipt: None,
        }
    }

    /// The review evidence this handoff carries, before-PR or
    /// before-landing, if any.
    pub fn evidence(&self) -> Option<&HandoffReviewEvidence> {
        match &self.disposition {
            HandoffReviewDisposition::BeforePr(evidence)
            | HandoffReviewDisposition::BeforeLanding(evidence) => Some(evidence.as_ref()),
            HandoffReviewDisposition::NotRequired => None,
        }
    }

    /// The timing the disposition records evidence for: `None` when it
    /// carries none.
    pub fn evidence_timing(&self) -> ReviewTiming {
        match &self.disposition {
            HandoffReviewDisposition::BeforePr(_) => ReviewTiming::BeforePr,
            HandoffReviewDisposition::BeforeLanding(_) => ReviewTiming::BeforeLanding,
            HandoffReviewDisposition::NotRequired => ReviewTiming::None,
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
    /// Criteria the task pilot typed post-deploy that this delivery owes
    /// until the deployed build is measured. The owner admits each against
    /// its own pilot finding when it accepts the handoff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owed_criteria: Vec<crate::workflow::OwedCriterion>,
    /// Culprit tasks the claimed implementer attributed this task's
    /// regression to (its `regression_from` output). A claimed worker cannot
    /// write its task's relations, so the owner checks each target against
    /// its own workspace and records the `regression_from` relations when it
    /// accepts the handoff.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regression_from: Vec<String>,
    /// [ORB-22807] Visual review was removed. A follower on an earlier build
    /// may still name its capture or resolution bundle here for a claim an
    /// earlier owner admitted; the owner accepts and ignores it. Delete the
    /// field after 2026-12-07.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visual: Option<HandoffArtifactRef>,
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
    /// [ORB-15427] Set on a failed command whose failing tests the leaf's
    /// own base run fails too. The owner accepts a non-zero log only with
    /// this report. When the host OSes match or either is unknown, its base
    /// run must share every failure named; when both known OSes differ, the
    /// owner checks base-branch history without running the base command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_red: Option<HandoffKnownRed>,
    /// Original failure and passing isolated replay on this exact candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaky: Option<crate::workflow::FlakyValidation>,
}

/// A claimed leaf's report that a required command failed only with tests
/// its base fails too [ORB-15427]. It is the leaf's claim, never proof. When
/// the host OSes match or either is unknown, the owner reruns the command on
/// this base before it accepts the log. When both OSes are known and differ,
/// the owner checks that the base is still on its captured base-branch history
/// and accepts the follower's reported failures for that OS without a base run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffKnownRed {
    /// The base commit the leaf compared with: the handed-off candidate's base.
    pub base_sha: String,
    /// What the candidate run reported testing, which a base run is handed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<serde_json::Value>,
    /// Every failure the candidate run's complete output names.
    pub failures: BTreeSet<String>,
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
