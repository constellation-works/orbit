use chrono::{DateTime, Utc};
pub use orbit_store::contracts::JobRunOrder;
use orbit_types::workflow::JobRunState;
use serde::Serialize;

/// Parameters for filtering and paging job run listings.
#[derive(Debug, Clone, Default)]
pub struct JobRunListParams {
    pub job_id: Option<String>,
    pub state: Option<JobRunState>,
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
    /// ends `cancelled` on its own.
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
}

impl JobRunCancelResult {
    pub(super) fn with_providers_stopped(mut self, stopped: usize) -> Self {
        self.provider_processes_stopped = stopped;
        self
    }
}
