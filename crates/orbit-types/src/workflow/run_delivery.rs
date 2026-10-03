//! The bounded delivery observation one task-delivery run produced for one
//! task: what the host committed and whether it verified a landing.
//!
//! This is the public read contract for consumers that must not hold operator
//! authority (plugins, ordinary agent sessions). Every value comes from a
//! host-run deterministic step's durable checkpoint; nothing here is read from
//! an agent response envelope, a task description, or a caller's claim.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::JobRunState;

/// Version of [`RunDeliveryObservation`]'s wire shape.
pub const RUN_DELIVERY_SCHEMA_VERSION: u32 = 1;

/// Where every non-null evidence field in an observation came from.
pub const RUN_DELIVERY_EVIDENCE_SOURCE: &str = "host_step_checkpoint";

/// One run's delivery evidence for one task, scoped to the answering workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDeliveryObservation {
    pub schema_version: u32,
    /// Workspace whose run and task stores answered.
    pub workspace_id: String,
    /// Stable repository identity: `owner/name` for a GitHub remote, otherwise
    /// `git:<digest>`. Null when the checkout cannot be inspected.
    pub repository: Option<String>,
    pub task_id: String,
    pub run_id: String,
    pub job_id: String,
    pub run_state: JobRunState,
    pub run_finished_at: Option<DateTime<Utc>>,
    /// Summary of `commit` and `landing`; never stronger than either.
    pub delivery_status: RunDeliveryStatus,
    pub commit: CommitObservation,
    pub landing: LandingObservation,
}

/// The overall answer a consumer branches on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunDeliveryStatus {
    /// A host landing step recorded a verified merge.
    Landed,
    /// The host committed a head, but no landing was verified (review-only
    /// delivery, a later failure, or a landing that was not requested).
    Committed,
    /// The host verified that the task needed no new commit.
    NoChange,
    /// The run has not reached a terminal outcome yet.
    InProgress,
    /// The run ended without committing anything for this task.
    NotDelivered,
    /// The durable evidence is missing, inconsistent or malformed.
    Unavailable,
}

/// What the host's `git_commit` step recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitObservation {
    pub status: CommitObservationStatus,
    /// Commit the run started from, as pinned by worktree setup.
    pub base_sha: Option<String>,
    /// Commit the host created. Present only for `committed`.
    pub head_sha: Option<String>,
    /// When the step finished, from the run's own step record.
    pub observed_at: Option<DateTime<Utc>>,
    pub provenance: Option<DeliveryEvidenceProvenance>,
    /// Why the status is `unavailable`.
    pub reason: Option<DeliveryEvidenceGap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitObservationStatus {
    /// The step created a new commit (`decision: performed`).
    Committed,
    /// Earlier steps of the same run already advanced HEAD past the base.
    AlreadyCommitted,
    VerifiedNoDiff,
    VerifiedAlreadyLanded,
    SkippedNoDiffExpected,
    /// The run is live and the step has not completed.
    Pending,
    /// The run ended before the step completed.
    NotReached,
    Unavailable,
}

impl CommitObservationStatus {
    /// The host verified that no new commit was needed.
    pub fn is_no_change(self) -> bool {
        matches!(
            self,
            Self::VerifiedNoDiff | Self::VerifiedAlreadyLanded | Self::SkippedNoDiffExpected
        )
    }
}

/// What the host's landing step recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingObservation {
    pub status: LandingObservationStatus,
    pub method: Option<LandingMethod>,
    /// Commit the merge produced on the target branch, when the host recorded
    /// one. A local fast-forward records none.
    pub landed_commit: Option<String>,
    pub pr_number: Option<u64>,
    pub observed_at: Option<DateTime<Utc>>,
    pub provenance: Option<DeliveryEvidenceProvenance>,
    pub reason: Option<DeliveryEvidenceGap>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingObservationStatus {
    Merged,
    /// Every landing step was skipped by its condition — e.g. a review-only
    /// run, or a no-change delivery with nothing to merge.
    NotRequested,
    /// The job has no landing step; a claimed run hands off to its owner.
    NotApplicable,
    Pending,
    NotReached,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandingMethod {
    PullRequest,
    LocalFastForward,
}

/// Which durable record answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryEvidenceProvenance {
    /// Always [`RUN_DELIVERY_EVIDENCE_SOURCE`].
    pub source: String,
    pub step_id: String,
    pub step_index: u32,
    pub activity: String,
}

/// Why evidence could not be read. Each refuses a guess rather than filling
/// the gap from a weaker source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryEvidenceGap {
    /// The job definition the run executed can no longer be loaded.
    JobDefinitionUnavailable,
    /// The job declares no host commit step.
    NoCommitStep,
    /// The run recorded no pipeline state.
    StateMissing,
    /// The step's checkpoint and the pipeline entry under its id disagree.
    CheckpointInconsistent,
    /// The checkpoint is not a recognized host step output.
    OutputMalformed,
    /// The checkpoint names a different task or run.
    OwnershipMismatch,
}
