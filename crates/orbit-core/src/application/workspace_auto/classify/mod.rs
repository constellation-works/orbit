mod admission;
mod forecast;
mod gap_fit;
mod lock_busy;
mod pool;
mod waiting;

/// Fail the next reservation release on this thread, for a test that needs
/// the release to lose to the host store while the pass's own failure stands.
#[cfg(feature = "test-support")]
pub(crate) mod release_failure_hook;

pub(crate) use admission::classify_workspace_auto_tasks;
pub(super) use pool::{DEFAULT_CANDIDATE_POOL, candidate_pool_limit, templated_u64};
