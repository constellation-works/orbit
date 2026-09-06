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
    /// `cancelled` when this request terminalized the run, or
    /// `already_terminal` when the run reached a durable terminal outcome
    /// before this request could do so.
    pub outcome: String,
    pub previous_state: String,
    pub final_state: String,
    pub actor: String,
    pub source: String,
    pub signal_attempted: bool,
    pub signal_outcome: Option<String>,
}
