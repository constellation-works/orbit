//! Failure-activity and final-recovery checkpoints.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::task::TaskStatus;
use crate::workflow::final_recovery::FinalRecoveryDecision;

/// Durable result of a job-level terminal failure activity.
///
/// A failure activity is not a successful workflow step, so its output cannot
/// live in `step_outputs`. Keeping it separately preserves the evidence needed
/// to resume from a recovery action without treating the failed step as
/// completed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FailureActivityCheckpoint {
    pub activity_name: String,
    pub failed_step_id: String,
    pub output: Value,
}

/// One weighted member of the pool an activity crew draw ran on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCrewPoolMember {
    pub name: String,
    pub weight: u32,
}

/// A crew an activity drew from a configured pool, frozen for the run.
///
/// An activity whose crew comes from a weighted pool (its `crew_config_key`)
/// draws once; every later dispatch of that key in the run, and every resume
/// seeded from it, reuses this record instead of rerolling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivityCrewDraw {
    /// Canonical crew name the draw selected.
    pub crew: String,
    /// The configuration the pool came from (`workflow.final_recovery_crews`).
    pub source: String,
    /// The permitted members and weights the draw ran on, so `run show`
    /// can explain the choice.
    pub eligible_pool: Vec<ActivityCrewPoolMember>,
}

/// The task revision final recovery observed when it was admitted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryObservedTask {
    pub status: TaskStatus,
    pub updated_at: DateTime<Utc>,
    /// Digest of the task's lifecycle content; absent on a checkpoint
    /// written before it was recorded, which compares by `updated_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_digest: Option<String>,
}

/// Idempotency key of a run's final-recovery decision: the run that admitted
/// it and that run's attempt. The applier names the run in the task comment
/// it writes, and a decision whose run already has that comment is not
/// applied again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryKey {
    pub run_id: String,
    pub attempt: u32,
}

/// Exact HEAD advance observed by the engine around final recovery dispatch.
/// It authorizes only this worktree's recorded repair, never later HEAD drift.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryRepairCommit {
    pub workspace_path: std::path::PathBuf,
    pub head_sha_before: String,
    pub head_sha: String,
}

/// A run's job-level final recovery [ORB-13907], recorded when it is admitted.
///
/// Admission writes this before the activity is dispatched, so a crash during
/// the activity, a resume decision, or an operator resume seeded from this
/// state never dispatches final recovery for the run a second time. The
/// decision is recorded here before it touches the task. A run resumed from
/// state that holds a decision other than `resume` applies that decision again
/// instead of dispatching, so a crash at any point converges on one outcome.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FinalRecoveryCheckpoint {
    pub key: FinalRecoveryKey,
    /// Top-level step whose failure admitted final recovery.
    pub failed_step_id: String,
    pub task_id: String,
    /// The task as it stood at admission; the applier refuses a decision once
    /// the task has changed since. Absent for a claimed leaf, whose task lives
    /// on its owner and is re-read there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<FinalRecoveryObservedTask>,
    /// Base ref a `complete_no_diff` commit must be reachable from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    pub admitted_at: DateTime<Utc>,
    /// The decision acted on, recorded before it is applied. The engine's
    /// substitutions (an invalid resume step, a failed activity) are recorded
    /// as the `escalate` they became.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<FinalRecoveryDecision>,
    /// A repair committed during recovery, recorded with the resume decision.
    /// Older run states carry no permission to accept a moved HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_commit: Option<FinalRecoveryRepairCommit>,
    /// What applying the decision did (`resume`, `settled: …` or
    /// `escalated: …`); absent while the decision is only intended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}
