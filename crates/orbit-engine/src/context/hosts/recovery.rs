//! Step and final recovery requests, admission, and decisions.

/// Resolved crew assignment from `config.toml`. Each field
/// is independently optional — the resolver in
/// `crate::activity_job::crew` falls back to the inline activity value
/// for any field the config does not specify.
///
/// String fields from the on-disk crew assignment are parsed into the
/// strongly typed activity-job enums at the orbit-core boundary; an
/// unrecognized provider yields `None` for that field rather than
/// silently coercing dispatch to a wrong runtime.
/// Whether a step-recovery hook may dispatch [ORB-11332].
///
/// `Allowed` is the pre-existing behavior for runs without an operation-mode
/// admission. `Reserved` names the aggregate-budget episode the host charged
/// before dispatch, and `Denied` carries the reason recovery must not run
/// (`recovery_episodes_exhausted`, `recovery_minutes_exhausted`,
/// `grant_revoked`). The original step error stays authoritative either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRecoveryAdmission {
    Allowed,
    Reserved { episode: u32 },
    Denied { reason: String },
}

/// What one admitted conflict recovery may complete, fixed before its provider
/// runs: the checkout, the HEAD the stopped rebase started from, and the
/// pinned base it continues onto.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseRecoveryAttemptScope {
    pub workspace_path: String,
    pub head_sha_before: String,
    pub target_base_sha: String,
}

/// Schema version of the decision file a `step_failure_recovery` invocation
/// writes into its [`StepRecoveryDecisionSlot`].
pub const STEP_RECOVERY_DECISION_SCHEMA_VERSION: u32 = 1;

/// The one `step_failure_recovery` invocation a decision slot is allocated
/// for: its run, failed step, failed attempt and assigned worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecoveryDecisionRequest {
    pub run_id: String,
    pub failed_step_id: String,
    pub attempt: u32,
    pub workspace_path: String,
}

/// A host-allocated, run-local file one recovery invocation may write its
/// decision to [ORB-14152].
///
/// The engine holds the slot in memory between dispatch and read-back, so
/// neither the agent nor any store it can write selects the path or the
/// binding. The nonce is fresh per invocation, so a decision written for any
/// other invocation cannot name this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecoveryDecisionSlot {
    pub run_id: String,
    pub failed_step_id: String,
    pub attempt: u32,
    pub nonce: String,
    /// Canonical root of the assigned worktree the slot lives under.
    pub workspace_root: std::path::PathBuf,
    /// Absolute path of the decision file, beneath `workspace_root`.
    pub path: std::path::PathBuf,
}

/// What a verified decision file tells the executor to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepRecoveryVerdict {
    /// Make the single post-recovery attempt of the failed step.
    Retry,
    /// Recovery could not repair the failure; return the original failure.
    NotRecovered,
    /// Something outside the run blocks the step [ORB-14268]. The step fails
    /// as an agent-declared blocker, so final recovery is skipped and the
    /// failure handoff blocks the task with the decision's kind.
    ExternalBlocker,
}

impl StepRecoveryVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::NotRecovered => "not_recovered",
            Self::ExternalBlocker => "external_blocker",
        }
    }
}

/// The host's reading of one decision slot after its invocation completed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRecoveryDecisionRead {
    /// Nothing was written. The executor retries only when it observes a
    /// change since the failure [ORB-14268].
    Absent,
    /// A well-formed decision bound to exactly this invocation.
    Verified {
        verdict: StepRecoveryVerdict,
        reason: Option<String>,
        /// The declared blocker; present exactly when `verdict` is
        /// [`StepRecoveryVerdict::ExternalBlocker`].
        blocker: Option<orbit_types::workflow::AgentBlocker>,
    },
    /// Something is at the slot but it is not a decision for this invocation:
    /// malformed, oversized, a link or non-regular file, or bound to another
    /// run, step, attempt or nonce. It authorizes nothing.
    Invalid { diagnostic: String },
}

/// [ORB-13907] What the engine asks a host before dispatching a job's final
/// recovery for a failed run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalRecoveryAdmissionRequest {
    pub task_id: String,
    /// Top-level step whose failure exhausted step recovery.
    pub failed_step_id: String,
    /// Base ref a `complete_no_diff` commit must be reachable from, when the
    /// run's worktree reported one.
    pub base_ref: Option<String>,
}

/// [ORB-13907] The host's answer to [`FinalRecoveryAdmissionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalRecoveryAdmission {
    /// Recorded durably for the run; the hook may run, and never again.
    Admitted,
    /// The hook does not run; today's failure path does.
    Skipped { reason: String },
}

/// [ORB-13907] A final-recovery decision for the host to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalRecoveryApplication {
    pub task_id: String,
    pub failed_step_id: String,
    /// The decision as the engine will act on it: a `resume` here already
    /// names a valid step, and an invalid one arrives as `escalate`.
    pub decision: orbit_types::workflow::FinalRecoveryDecision,
    /// Top-level index a `resume` reruns from. Durable step checkpoints at and
    /// after it are stale once the run goes back there.
    pub resume_step_index: Option<u32>,
    /// Engine-observed HEAD advance during this recovery dispatch. Never read
    /// from the recovery activity's response.
    pub repair_commit: Option<orbit_types::workflow::FinalRecoveryRepairCommit>,
    /// The run's assigned worktree, where a `complete_no_diff` commit is
    /// resolved.
    pub workspace_path: std::path::PathBuf,
    /// Whether the run held `completion: done` authority.
    pub completion_done: bool,
}

/// [ORB-13907] What applying a final-recovery decision did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalRecoveryApplied {
    /// Recorded; the engine reruns from the decision's step.
    Resume,
    /// The task was settled (completed, rejected, archived, requeued, or
    /// handed to the claim settlement); the run ends without its
    /// `failure_activity`.
    Settled { outcome: String },
    /// The task is parked for a human — by the decision, by the applier's
    /// override, or because the applier refused it; the run's
    /// `failure_activity` follows.
    Escalated { outcome: String },
}
