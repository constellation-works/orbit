mod cleanup;
mod dependency_delivery;
mod gc;
mod identity;
mod merge;
mod paths;
mod reclaim;
mod registration;
mod rescue;
mod setup;

pub use identity::WORKTREE_IDENTITY_INPUT_KEYS;
pub(in crate::executor::automation) use identity::WorktreeIdentity;
use identity::require_run_id;
pub use paths::resolve_shared_worktree_path;
pub(crate) use paths::scratch_checkout_path;
pub(super) use registration::{
    is_registered_worktree, path_is_registered, registered_worktree_paths,
};

pub(in crate::executor::automation::vcs) use merge::{
    checkout_holding_branch, ensure_clean_checkout,
};

#[cfg(all(
    any(target_os = "linux", target_os = "macos"),
    feature = "test-support"
))]
pub use gc::with_host_target_removal_hook;
pub use gc::{
    WorktreeGcOptions, WorktreeGcResult, collect_worktrees, run_worktree_has_reclaim_output,
    run_worktree_paths,
};
pub(in crate::executor::automation) use merge::merge_batch_worktree_into_base;
pub use reclaim::WorktreeReclaimReport;
pub use rescue::WorktreeGcRecovery;
pub(in crate::executor::automation) use setup::setup_worktree;

#[cfg(test)]
mod tests;
