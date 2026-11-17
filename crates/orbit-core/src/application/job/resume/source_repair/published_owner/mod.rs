//! Publication survives a final-recovery requeue independently of failure handoff.
//!
//! Two publications are preserved this way: the run's own pipeline push and
//! PR, which a final-recovery `requeue` keeps, and the `[BLOCKED]` PR a
//! failure handoff published, which an operator or cleared-infra requeue of
//! the blocked task keeps (`failure_handoff`). Both are re-authenticated in
//! full from the immutable source when the next run adopts them.

mod candidate;
mod requeue;
mod source_checks;

use serde::{Deserialize, Serialize};

pub(super) use source_checks::{footprint_held, require_stopped_failed_requeue_source};

const PRESERVED: &str = "published_owner_requeued";

#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PreservedPublication {
    schema_version: u32,
    source_run_id: String,
    task_digest: String,
    /// The publication is the source's failure-handoff PR, authenticated
    /// from its handoff receipt rather than pipeline checkpoints.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    failure_handoff: bool,
}

/// A `backlog` task whose next run resumes the publication a final-recovery
/// requeue preserved: it adopts the existing branch and pull request and
/// completes them. See [`OrbitRuntime::published_resume`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PublishedResume {
    /// The stopped run whose publication the next run adopts.
    pub(crate) source_run_id: String,
    /// The pull request that run opened, when its `pr_open` checkpoint
    /// recorded one.
    pub(crate) pull_request: Option<String>,
}
