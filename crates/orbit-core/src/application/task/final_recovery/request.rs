//! Final-recovery completion authority, requeue bound, request and outcome.

use std::path::PathBuf;

use chrono::Duration;
use orbit_types::task::{RecordedFinalRecovery, TaskStatus};

use super::revision::FinalRecoveryTaskRevision;

/// How far a verified `complete_no_diff` may take the task: the failed run's
/// own completion authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalRecoveryCompletion {
    /// The run delivered to review; an operator approves.
    Review,
    /// The run was authorized to complete the task.
    Done,
}

/// How many final-recovery requeues one task may take inside a window before
/// a further `requeue` escalates instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalRecoveryRequeueBound {
    /// Requeues allowed inside `window`.
    pub max_requeues: usize,
    /// Trailing window the requeues are counted over.
    pub window: Duration,
}

impl Default for FinalRecoveryRequeueBound {
    fn default() -> Self {
        Self {
            max_requeues: 2,
            window: Duration::hours(24),
        }
    }
}

/// Everything the applier needs besides the decision itself.
#[derive(Debug, Clone)]
pub struct FinalRecoveryRequest {
    /// The task the failed run carried.
    pub task_id: String,
    /// The failed run; named in every comment the applier writes.
    pub run_id: String,
    /// The task revision observed when the run failed.
    pub observed: FinalRecoveryTaskRevision,
    /// Checkout `evidence_commit` is resolved in.
    pub repo_root: PathBuf,
    /// Base branch ref a `complete_no_diff` commit must be reachable from.
    pub base_ref: String,
    /// The run's completion authority.
    pub completion: FinalRecoveryCompletion,
    /// Requeue bound; [`FinalRecoveryRequeueBound::default`] is 2 per 24 h.
    pub requeue_bound: FinalRecoveryRequeueBound,
}

/// What the applier did with a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalRecoveryOutcome {
    /// `resume` is the workflow engine's to apply; nothing was written.
    Resume {
        /// Step the agent asked to resume from.
        step_id: String,
    },
    /// The task moved to `status` on the verified covering commit.
    Completed {
        /// `review` or `done`, per the run's completion authority.
        status: TaskStatus,
        /// Full SHA of the verified covering commit.
        evidence_commit: String,
    },
    /// The task was rejected.
    Rejected,
    /// The task was archived.
    Archived,
    /// The task went back to `backlog`.
    Requeued,
    /// The task was blocked for a human. `reason` says why when the applier
    /// escalated a decision it could not apply as proposed.
    Escalated {
        /// The applier's own reason, when it overrode the proposal.
        reason: Option<String>,
    },
    /// Nothing changed but the audit comment.
    Refused {
        /// Why the decision was not applied.
        reason: String,
    },
}

impl FinalRecoveryOutcome {
    /// The outcome as the typed decision records it; `None` for `resume`,
    /// which the applier never records.
    pub(super) fn applied(&self) -> Option<RecordedFinalRecovery> {
        Some(match self {
            Self::Resume { .. } => return None,
            Self::Completed {
                status,
                evidence_commit,
            } => RecordedFinalRecovery::Completed {
                status: *status,
                evidence_commit: evidence_commit.clone(),
            },
            Self::Rejected => RecordedFinalRecovery::Rejected,
            Self::Archived => RecordedFinalRecovery::Archived,
            Self::Requeued => RecordedFinalRecovery::Requeued,
            Self::Escalated { reason } => RecordedFinalRecovery::Escalated {
                reason: reason.clone(),
            },
            Self::Refused { reason } => RecordedFinalRecovery::Refused {
                reason: reason.clone(),
            },
        })
    }
}

impl From<RecordedFinalRecovery> for FinalRecoveryOutcome {
    fn from(applied: RecordedFinalRecovery) -> Self {
        match applied {
            RecordedFinalRecovery::Completed {
                status,
                evidence_commit,
            } => Self::Completed {
                status,
                evidence_commit,
            },
            RecordedFinalRecovery::Rejected => Self::Rejected,
            RecordedFinalRecovery::Archived => Self::Archived,
            RecordedFinalRecovery::Requeued => Self::Requeued,
            RecordedFinalRecovery::Escalated { reason } => Self::Escalated { reason },
            RecordedFinalRecovery::Refused { reason } => Self::Refused { reason },
        }
    }
}
