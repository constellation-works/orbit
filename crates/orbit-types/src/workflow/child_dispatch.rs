//! Durable record of a child job run dispatched by a parent run's step.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which lifecycle phase a child dispatch has reached in the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildDispatchPhase {
    /// `orbit.pipeline.invoke` returned a durable child run id. The parent has
    /// recorded the linkage but has not yet entered its blocking wait.
    Submitted,
    /// The parent is blocked on this child's terminal state.
    Waiting,
    /// The parent stopped tracking this child: the wait returned, the dispatch
    /// was detached by design, or the parent itself terminalized.
    Terminal,
}

impl ChildDispatchPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ChildDispatchPhase::Submitted => "submitted",
            ChildDispatchPhase::Waiting => "waiting",
            ChildDispatchPhase::Terminal => "terminal",
        }
    }

    /// Whether the parent still considers this dispatch open. An open dispatch
    /// on a terminal parent is the exact inconsistency this record exists to
    /// prevent, so cancellation closes every one it finds.
    pub fn is_open(self) -> bool {
        !matches!(self, ChildDispatchPhase::Terminal)
    }
}

/// What a terminalizing parent does to a child it dispatched.
///
/// The policy is decided by how the child was dispatched, not by the caller of
/// the moment, so it is the same for an operator cancel, a dashboard cancel,
/// and a cascade from a grandparent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildCancellationPolicy {
    /// The parent was blocking on this child, so the child's only consumer is
    /// gone. Cancelling the parent cancels the child.
    Cascade,
    /// The child was dispatched detached, explicitly to outlive the parent's
    /// step. Cancelling the parent leaves it running.
    Detach,
}

impl ChildCancellationPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            ChildCancellationPolicy::Cascade => "cascade",
            ChildCancellationPolicy::Detach => "detach",
        }
    }
}

/// What actually happened when the policy was applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildCancellation {
    pub policy: ChildCancellationPolicy,
    /// `detached` (the `detach` policy left the child running), or
    /// `cancelled`, `skipped` or `failed` under `cascade` — the observed
    /// result, which may differ from the policy's intent when the child had
    /// already terminalized.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub at: DateTime<Utc>,
}

/// One child Run a parent step submitted, and how far the parent got with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildDispatch {
    /// The exact run id returned by `orbit.pipeline.invoke`. Never inferred
    /// from task status or timestamps.
    pub child_run_id: String,
    pub job_name: String,
    /// The parent job step that dispatched this child, when the engine
    /// supplied it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_step_id: Option<String>,
    /// The deterministic action that dispatched it (`invoke_and_wait` or
    /// `invoke_detached`).
    pub action: String,
    /// Whether the parent blocks on this child's terminal state. Drives the
    /// cancellation policy.
    pub blocking: bool,
    /// Whether the child was admitted immediately or held behind
    /// `max_active_runs` at submission time.
    pub queued: bool,
    pub submitted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub phase: ChildDispatchPhase,
    /// The child's terminal status once the parent observed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancellation: Option<ChildCancellation>,
}

impl ChildDispatch {
    /// A freshly submitted child, in the `Submitted` phase. Callers move it to
    /// `Waiting` only once they are actually about to block.
    pub fn submitted(
        child_run_id: String,
        job_name: String,
        action: String,
        blocking: bool,
        queued: bool,
        submitted_at: DateTime<Utc>,
    ) -> Self {
        Self {
            child_run_id,
            job_name,
            parent_step_id: None,
            action,
            blocking,
            queued,
            submitted_at,
            updated_at: submitted_at,
            phase: ChildDispatchPhase::Submitted,
            child_status: None,
            error: None,
            cancellation: None,
        }
    }

    pub fn with_parent_step_id(mut self, parent_step_id: Option<String>) -> Self {
        self.parent_step_id = parent_step_id;
        self
    }

    /// The policy this dispatch's shape implies for a terminalizing parent.
    pub fn cancellation_policy(&self) -> ChildCancellationPolicy {
        if self.blocking {
            ChildCancellationPolicy::Cascade
        } else {
            ChildCancellationPolicy::Detach
        }
    }
}
