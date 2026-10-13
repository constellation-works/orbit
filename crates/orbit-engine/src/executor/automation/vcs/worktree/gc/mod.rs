mod classify;
mod leftovers;
mod paths;
mod reclaim;
mod rescue;
mod sweep;

pub use paths::run_worktree_paths;
pub use reclaim::run_worktree_has_reclaim_output;
// Keep the report type reachable at the existing GC module path.
#[allow(unused_imports)]
pub use sweep::WorktreeGcReport;
pub use sweep::{WorktreeGcOptions, WorktreeGcResult, collect_worktrees};
