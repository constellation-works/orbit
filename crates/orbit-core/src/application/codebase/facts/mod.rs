//! The workspace scan behind the codebase analytics: one pass over every task
//! (envelope plus history), the reservation history and the lock-conflict
//! denials, reduced to the per-task facts the rankings slice by window.
//!
//! Lock waits are read from the same records: a task's recorded denials say
//! when it started waiting, and its next reservation grant (or its leaving
//! the backlog) says when the wait ended.
//!
//! What a task's history, comments and failed claim settlements say about it
//! ([`HistoryFacts`]) is kept in the store between scans (see [`super::stored`]),
//! so a scan reads only the bundles of tasks that changed since. Envelope
//! facts come from the task index at every scan. A caller that serves
//! repeated requests (the dashboard) also caches the facts and slices every
//! window, threshold and rollup from them; the CLI and the tool scan once
//! per call.

mod block_cause;
mod history;
mod lock_denial;
mod scan;
mod types;

pub(in crate::application) use block_cause::{
    SettledFailure, actor_kind, classify_block, failed_claim_settlements,
};
pub(super) use history::HistoryFacts;
pub use scan::CodebaseFacts;
pub(super) use scan::{ReservationFacts, TaskFacts, scan, scan_regressions};
pub(super) use types::{ACTOR_KINDS, Block, INFRA_CAUSES};
pub(in crate::application) use types::{Anchor, BlockCause};
