//! Tool-invocation metrics derived from job-run traces.
//!
//! Migrated out of the decommissioned `orbit-knowledge` crate in ORB-00391.
//! The persisted [`orbit_types::workflow::KnowledgeRunMetrics`] type and the
//! `JobRun::knowledge_metrics` field stay in `orbit-common`; this module owns
//! the pure computation over invocation traces ([`merge_invocation_trace`])
//! and the cross-run aggregation rendered by the dashboard ([`aggregate`]).
//!
//! The v1 pack-compression path was dropped with the tool (ORB-00388).
//! ORB-10828 retired the last builtin that fed read-token counters, so ingest
//! no longer creates knowledge metrics from new traces. Dashboard aggregation
//! still reads historical `double_read_rate` values from persisted job runs.

mod ingest;
pub mod reliability;
mod summary;

pub(crate) use ingest::merge_invocation_trace;
pub use summary::{
    DoubleReadSummary, KnowledgeStatsSummary, RatioSummary, TokenInputSummary, aggregate,
};
