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

/// Resolve a pull request base tip and, when requested, its fork from the head.
pub(super) fn resolve_pr_base(
    workspace: &std::path::Path,
    input: &serde_json::Value,
    base_name: &str,
    head: Option<&str>,
) -> Result<(Option<String>, String, String), String> {
    let sync = base_sync_mode_from_input(input).map_err(|error| error.to_string())?;
    let base_ref = resolve_worktree_start_point(workspace, base_name, sync)
        .map_err(|error| format!("base '{base_name}' could not be resolved: {error}"))?;
    let tip =
        super::freshness::commit_sha(workspace, &base_ref).map_err(|error| error.to_string())?;
    let fork = head
        .map(|head| {
            git_output(workspace, &["merge-base", head, &tip])
                .map(|base| base.trim().to_string())
                .map_err(|error| format!("the fork point of {head} could not be read: {error}"))
        })
        .transpose()?;
    let branch = base_name
        .strip_prefix("origin/")
        .unwrap_or(base_name)
        .to_string();
    Ok((fork, branch, tip))
}
