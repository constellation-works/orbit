use chrono::{DateTime, Utc};
pub use orbit_store::contracts::JobRunOrder;
use orbit_types::workflow::JobRunState;
use serde::Serialize;

/// Parameters for filtering and paging job run listings.
#[derive(Debug, Clone, Default)]
pub struct JobRunListParams {
    pub job_id: Option<String>,
    /// Exact string membership in the run's submitted `input.task_ids` array,
    /// or equality with a text top-level `input.task_id`. Applied before
    /// ordering, limiting and row/step hydration. Missing or non-array
    /// `task_ids`, non-text `task_id`, and nested fields do not match; other
    /// input fields confer no ownership.
    pub task_id: Option<String>,
    pub state: Option<JobRunState>,
    /// Match any listed state; empty means unrestricted. Combined with state as an intersection.
    pub states: Vec<JobRunState>,
    /// Restrict results to every state considered terminal by `JobRunState`.
    ///
    /// This is independent from `state` so existing callers can continue to
    /// request one concrete state without changing their query semantics.
    pub terminal_only: bool,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    /// Which timestamp `limit` truncates against. Defaults to `CreatedAt` so
    /// existing CLI/history callers keep their current ordering.
    pub order_by: JobRunOrder,
}

/// Result of a job run cancellation attempt.
#[derive(Debug, Clone, Serialize)]
pub struct JobRunCancelResult {
    pub run_id: String,
    /// `cancelled` when this request terminalized the run,
    /// `already_terminal` when the run reached a durable terminal outcome
    /// before this request could do so, or `cancelling` when a pull drain
    /// stopped admitting and is waiting for its launched leaves before it
    /// ends `cancelled` on its own. This describes the parent run; callers
    /// must also inspect `unstopped_leaves` and `unstopped_children` before
    /// treating a forced cancellation as complete.
    pub outcome: String,
    pub previous_state: String,
    pub final_state: String,
    pub actor: String,
    pub source: String,
    pub signal_attempted: bool,
    pub signal_outcome: Option<String>,
    /// Provider CLI processes this cancellation stopped. They run in their own
    /// process groups, so the owner signal never reaches them.
    pub provider_processes_stopped: usize,
    /// [ORB-13663] Pull settlements this cancellation carried: for a follower
    /// pull drain, every admission a settle-only pass touched (its unlaunched
    /// claims are released back to the owner's backlog once no drain carries
    /// them); for a claimed leaf, its own settlement. Empty for every other
    /// run.
    pub pull_settlements: Vec<crate::application::distributed::PullSettlementEntry>,
    /// The claimed leaves a `cancelling` pull drain is waiting for.
    pub waiting_leaves: Vec<crate::application::distributed::DrainClaimedLeaf>,
    /// Runs a forced cancel stopped besides this one: a pull drain's live
    /// claimed leaves, or a local drain's detached children.
    pub forced_runs: Vec<String>,
    /// Live claimed leaves a forced cancel could not confirm it stopped.
    /// Their claims were not handed back: the owner keeps each until the
    /// leaf is seen to stop. Any entry makes the forced cancel a failure.
    pub unstopped_leaves: Vec<UnstoppedLeaf>,
    /// Detached local-drain children whose stop could not be confirmed.
    /// Any entry makes the forced cancel incomplete.
    pub unstopped_children: Vec<UnstoppedChild>,
}

/// A detached child a forced local-drain cancel could not confirm stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnstoppedChild {
    pub child_run_id: String,
    /// Why the child could not be confirmed stopped.
    pub reason: String,
}

impl UnstoppedChild {
    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{}: {}", self.child_run_id, self.reason)
    }
}

/// A claimed leaf a forced drain cancel could not confirm it stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnstoppedLeaf {
    pub leaf_run_id: String,
    /// The owner's task the leaf executes.
    pub task_id: Option<String>,
    /// Why the stop is unconfirmed, and what became of the claim.
    pub reason: String,
}

impl UnstoppedLeaf {
    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (leaf {}): {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.leaf_run_id,
            self.reason
        )
    }
}

impl JobRunCancelResult {
    pub(super) fn with_providers_stopped(mut self, stopped: usize) -> Self {
        self.provider_processes_stopped = stopped;
        self
    }
}
