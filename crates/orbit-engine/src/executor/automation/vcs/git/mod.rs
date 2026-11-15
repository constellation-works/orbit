mod command;
mod fetch;
mod timeout;

pub use command::BaseSyncMode;
pub(crate) use command::{
    GitBytesOutcome, GitOutcome, direct_remote_target, git_command_success, git_failure_error,
    git_output, git_output_paths, git_output_raw, git_output_with_env, git_request, git_run,
    git_run_bytes, git_success, is_ancestor, verify_direct_remote_target,
};
pub(in crate::executor::automation) use command::{base_sync_mode_from_input, gh_environment};
pub use fetch::fetch_remote_base;
#[cfg(test)]
pub(crate) use fetch::fetch_remote_base_within;
#[cfg(feature = "test-support")]
pub use fetch::with_worktree_setup_fetch_limits;
pub(in crate::executor::automation) use fetch::{
    normalize_base_branch, resolve_worktree_start_point, resolve_worktree_start_point_for_admission,
};
pub(crate) use timeout::{
    GitDeadlineGuard, GitTimeoutBudget, GitTimeoutBudgetGuard, git_timeout_error,
    timeout_recovery_error,
};
