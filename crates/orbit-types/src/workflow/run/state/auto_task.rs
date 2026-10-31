//! The versioned checkpoint of a job with `auto-task:` steps
//! (`docs/design/activity-job/specs/auto-task-step.md` §5): the frozen
//! definitions, admission time and source, each step's activation, action
//! key, binding and receipt, the parked continuation, and the branches of a
//! parallel block that already succeeded.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The checkpoint layout this binary reads and writes. A stored checkpoint
/// with a higher version was written by a newer binary: this one refuses to
/// rewrite it rather than drop obligations it cannot represent.
pub const AUTO_TASK_CHECKPOINT_VERSION: u32 = 1;

/// Durable state of one execution of a job with auto-task steps.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutoTaskRunCheckpoint {
    /// Checkpoint layout version; see [`AUTO_TASK_CHECKPOINT_VERSION`].
    pub version: u32,
    /// The job definition's YAML as admitted. The run executes this pinned
    /// copy; a definition edit affects later executions only.
    pub frozen_job: String,
    /// Each named auto-task definition as first resolved by this execution,
    /// keyed by definition name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub frozen_targets: BTreeMap<String, FrozenAutoTaskTarget>,
    /// Admission time: `job.started_at` and the origin of every wait clock.
    pub started_at: DateTime<Utc>,
    /// The workspace source revision pinned at admission, when one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    /// The finite outer deadline `spec.expire_when` resolves to.
    pub expires_at: DateTime<Utc>,
    /// Per-step records keyed by step id (step ids are unique in a job).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub steps: BTreeMap<String, AutoTaskStepRecord>,
    /// Recorded value of each parallel branch that already succeeded, keyed
    /// by the parallel step id and then the branch id. A replay restores
    /// the branch instead of running it again. A target branch stores its
    /// raw output. A compound branch stores
    /// `{kind: "compound_branch", output, exposed}`: `output` is the branch
    /// output and `exposed` is the other pipeline entries it wrote (nested
    /// step outputs and fan-in aliases). A compound checkpoint from before
    /// that envelope stores only the raw output.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub branches: BTreeMap<String, BTreeMap<String, Value>>,
    /// Present while the execution is deliberately parked: no worker holds
    /// it, and the owner clock resumes it at `wake_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<ParkedWait>,
}

impl AutoTaskRunCheckpoint {
    /// Whether this binary can rewrite the checkpoint without losing state.
    pub fn is_supported(&self) -> bool {
        self.version <= AUTO_TASK_CHECKPOINT_VERSION
    }

    /// Whether the parked execution is due at `now`.
    pub fn parked_due(&self, now: DateTime<Utc>) -> bool {
        self.parked.as_ref().is_some_and(|park| park.wake_at <= now)
    }
}

/// An auto-task definition as an execution first resolved it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrozenAutoTaskTarget {
    /// SHA-256 of the definition file's bytes.
    pub digest: String,
    /// The resolved definition (operator settings applied) as JSON, which
    /// this execution mints from.
    pub source: String,
    /// When this execution first resolved it.
    pub resolved_at: DateTime<Utc>,
}

/// An auto-task step's observed status (spec §2). `Done` is an accepted
/// completion, distinct from the task's raw status.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AutoTaskStepStatus {
    Waiting,
    Queued,
    Running,
    Done,
    Held,
    Failed,
    Timeout,
    Cancelled,
    Skipped,
}

impl AutoTaskStepStatus {
    /// The CEL status literal.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Held => "held",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Skipped => "skipped",
        }
    }

    /// Whether the step settled and never changes again in this execution.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Timeout | Self::Cancelled | Self::Skipped
        )
    }
}

/// One auto-task step's durable record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoTaskStepRecord {
    /// The auto-task definition the step names.
    pub auto_task: String,
    /// When job structure activated the step; persisted once, never reset.
    pub activated_at: DateTime<Utc>,
    /// `(run lineage, step id)` reservation key a mint is resolved by.
    pub action_key: String,
    /// Digest of the definition and inputs the step requests.
    pub input_digest: String,
    pub status: AutoTaskStepStatus,
    /// When the step reserved its mint, durably, before any external effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_at: Option<DateTime<Utc>>,
    /// The bound task, once bound or minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Whether this step minted the bound task (`false`: bound open work).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub minted: bool,
    /// The bound task's accepted or refused settlement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<AutoTaskReceipt>,
    /// When the step settled in the job (`self.prev.end_time`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<DateTime<Utc>>,
    /// Why the step is waiting, held or ended, for run detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}

/// A bound task's terminal observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoTaskReceipt {
    pub task_id: String,
    /// The task's raw terminal status.
    pub task_status: String,
    /// Whether the completion contract accepted it.
    pub accepted: bool,
    /// When the owner's store recorded the terminal transition.
    pub settled_at: DateTime<Utc>,
    /// When this execution first observed it.
    pub observed_at: DateTime<Utc>,
}

/// Why and until when a job execution is parked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ParkedWait {
    pub parked_at: DateTime<Utc>,
    /// When the owner clock resumes the execution.
    pub wake_at: DateTime<Utc>,
    /// The waiting steps and what each waits for.
    pub reason: String,
}
