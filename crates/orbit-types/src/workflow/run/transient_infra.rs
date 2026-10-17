//! Failures Orbit's own coordination infrastructure caused, not the candidate.
//!
//! A deterministic delivery step that lost only to a lock judged nothing
//! about the work: the same call succeeds unchanged once the holder lets go.
//! Final recovery classifies such a failure without an agent, and the task
//! waits blocked on that condition until a probe shows it cleared (see
//! `orbit task recheck-blocked`). A claimed worker that could not reach its
//! owner (`[owner_route_unavailable]`) is not classified here: final
//! recovery already skips it and its claim is released.
//!
//! Classification reads the action's own error, never text an agent or a
//! validation command produced: only a deterministic delivery action listed
//! in `DELIVERY_ACTIONS` counts (no validation action is listed, since its
//! failure embeds command output), and the error has to *start* with one of
//! the typed displays below, so a test log that merely quotes "database is
//! locked" is not a lock failure. A claimed leaf is never classified: its
//! task belongs to its owner, which settles the claim through its own
//! recovery.

use serde::{Deserialize, Serialize};

/// History event final recovery records, by `system`, beside the block it
/// applies for a [`TransientInfrastructure`] failure. Its note is a
/// [`TransientInfraBlock`].
pub const TRANSIENT_INFRA_BLOCKED_EVENT: &str = "transient_infra_blocked";

/// History event recorded, by `system`, when a transient infrastructure
/// block is requeued because its probe passed. Its note is a
/// [`TransientInfraRequeue`]; the count per publication bounds the repeats.
pub const TRANSIENT_INFRA_REQUEUED_EVENT: &str = "transient_infra_requeued";

/// History event recorded, by `system`, when the cleared-infra requeue could
/// not authenticate a blocked task's publication. The block then waits for an
/// operator instead of being re-authenticated on every clock tick. Its note
/// says why.
pub const TRANSIENT_INFRA_REQUEUE_REFUSED_EVENT: &str = "transient_infra_requeue_refused";

/// Automatic requeues one candidate publication (or, unpublished, one task)
/// may take for transient infrastructure before it stays blocked for an
/// operator.
pub const MAX_TRANSIENT_INFRA_REQUEUES: usize = 2;

/// Deterministic delivery actions whose own failure may be classified. They
/// move or record the candidate (worktree, commit, branch, push, pull
/// request, review gate, completion) and judge nothing about it, so a lock
/// they lost is Orbit's. Validation actions are deliberately absent: their
/// failure embeds command output, whatever that output says.
const DELIVERY_ACTIONS: &[&str] = &[
    "worktree_setup",
    "reserve_locks",
    "candidate_resume",
    "git_commit",
    "pr_prepare",
    "git_rebase",
    "git_push",
    "pr_open",
    "pr_promote",
    "review_gate_admit",
    "review_gate_settle",
    "pr_complete",
    "git_merge",
    "task_complete",
    "release_locks",
];

/// Which infrastructure condition failed a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransientInfrastructure {
    /// A file lock or a remote owner's lock stayed held past its deadline.
    LockBusy,
    /// The SQLite store stayed busy or locked past its busy timeout.
    StoreLocked,
}

impl TransientInfrastructure {
    /// Stable snake-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LockBusy => "lock_busy",
            Self::StoreLocked => "store_locked",
        }
    }

    /// The condition in words, for notes and reports.
    pub fn describe(self) -> &'static str {
        match self {
            Self::LockBusy => "a lock stayed held past its acquisition deadline",
            Self::StoreLocked => "the task store stayed locked past its busy timeout",
        }
    }

    /// The class of deterministic action `action`'s own failure `message`,
    /// when that failure is transient infrastructure.
    #[must_use]
    pub fn of_action_failure(action: &str, message: &str) -> Option<Self> {
        if !DELIVERY_ACTIONS.contains(&action) {
            return None;
        }
        let message = message.trim_start();
        let message = message
            .strip_prefix("execution failed: ")
            .unwrap_or(message);
        // `OrbitError::RemoteTool` from an owner that answered `lock_busy`.
        if message.starts_with("remote tool failed (lock_busy)") {
            return Some(Self::LockBusy);
        }
        // `OrbitError::FileLockTimeout`.
        if let Some(rest) = message.strip_prefix("timed out after ")
            && rest.contains("ms acquiring ")
            && rest.contains(" lock '")
        {
            return Some(Self::LockBusy);
        }
        // `OrbitError::SqliteContention`, and SQLite's own busy/locked text
        // carried by `OrbitError::Store`.
        if let Some(rest) = message.strip_prefix("store error: ")
            && (rest.starts_with("database is locked")
                || rest.starts_with("database table is locked")
                || (rest.starts_with("SQLite database '")
                    && rest.contains(" remained locked during ")))
        {
            return Some(Self::StoreLocked);
        }
        None
    }
}

/// The note of a [`TRANSIENT_INFRA_BLOCKED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransientInfraBlock {
    pub schema_version: u32,
    /// The run whose step failed.
    pub run_id: String,
    pub failed_step_id: String,
    pub class: TransientInfrastructure,
}

/// The note of a [`TRANSIENT_INFRA_REQUEUED_EVENT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransientInfraRequeue {
    pub schema_version: u32,
    /// The run whose block was requeued.
    pub run_id: String,
    pub class: TransientInfrastructure,
    /// The pull request the requeued candidate is published on, if any; the
    /// bound counts requeues per publication.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<String>,
}
