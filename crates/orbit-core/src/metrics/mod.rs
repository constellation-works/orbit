//! Metrics computed from persisted job-run data.
//!
//! Migrated out of the decommissioned `orbit-knowledge` crate in ORB-00391.
//! The persisted [`orbit_types::workflow::KnowledgeRunMetrics`] type and the
//! `JobRun::knowledge_metrics` field stay in `orbit-common`; this module owns
//! cross-run aggregation of historical knowledge metrics rendered by the
//! dashboard ([`aggregate`]).
//!
//! The v1 pack-compression path was dropped with the tool (ORB-00388).
//! ORB-10828 retired the last builtin that fed read-token counters. Dashboard
//! aggregation still reads historical `double_read_rate` values from persisted
//! job runs.

pub use orbit_runtime::cost;
pub mod reliability;
mod summary;

pub use summary::{
    DoubleReadSummary, KnowledgeStatsSummary, RatioSummary, TokenInputSummary, aggregate,
};
