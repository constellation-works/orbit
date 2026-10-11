//! Branch divergence and commit/ref lookup.
use super::super::git::{BaseSyncMode, git_output, resolve_worktree_start_point};
use orbit_common::OrbitError;
use std::path::Path;

#[derive(Debug, Clone)]
pub(in crate::executor::automation::vcs) struct BranchFreshness {
    pub(in crate::executor::automation::vcs) base_ref: String,
    pub(in crate::executor::automation::vcs) head_ref: String,
    pub(in crate::executor::automation::vcs) commits_behind: u64,
    pub(in crate::executor::automation::vcs) commits_ahead: u64,
}

pub(in crate::executor::automation::vcs) fn original_base_sha(
    workspace_path: &Path,
    head_sha: &str,
    target_base_sha: &str,
) -> Result<String, OrbitError> {
    match git_output(workspace_path, &["merge-base", head_sha, target_base_sha]) {
        Ok(sha) if !sha.trim().is_empty() => Ok(sha.trim().to_string()),
        _ => Ok(
            git_output(workspace_path, &["rev-parse", &format!("{head_sha}^")])?
                .trim()
                .to_string(),
        ),
    }
}

pub(in crate::executor::automation::vcs) fn ensure_branch_fresh_against_base(
    repo_root: &Path,
    head: &str,
    base: &str,
    sync_mode: BaseSyncMode,
) -> Result<BranchFreshness, OrbitError> {
    let base_ref = resolve_worktree_start_point(repo_root, base, sync_mode)?;
    let base_sha = commit_sha(repo_root, &base_ref)?;
    let freshness = branch_freshness_against_ref(repo_root, head, &base_ref, &base_sha)?;

    if freshness.commits_behind > 0 {
        return Err(OrbitError::Execution(format!(
            "task branch '{head}' is behind base '{base_ref}' by {} commit(s); refresh the task branch before opening or merging the PR",
            freshness.commits_behind
        )));
    }
    Ok(freshness)
}

pub(in crate::executor::automation::vcs) fn branch_freshness_against_ref(
    repo_root: &Path,
    head: &str,
    base_ref: &str,
    base_sha: &str,
) -> Result<BranchFreshness, OrbitError> {
    let divergence = git_output(
        repo_root,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base_sha}...{head}"),
        ],
    )?;
    let mut parts = divergence.split_whitespace();
    let commits_behind = parse_divergence_count(parts.next(), "behind", base_ref, head)?;
    let commits_ahead = parse_divergence_count(parts.next(), "ahead", base_ref, head)?;
    if parts.next().is_some() {
        return Err(OrbitError::Execution(format!(
            "unexpected git divergence output while comparing '{head}' to '{base_sha}': {divergence}"
        )));
    }
    Ok(BranchFreshness {
        base_ref: base_ref.to_string(),
        head_ref: head.to_string(),
        commits_behind,
        commits_ahead,
    })
}

pub(in crate::executor::automation::vcs) fn commit_sha(
    repo_root: &Path,
    reference: &str,
) -> Result<String, OrbitError> {
    Ok(git_output(
        repo_root,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )?
    .trim()
    .to_string())
}

pub(in crate::executor::automation::vcs) fn remote_branch_sha(
    repo_root: &Path,
    branch: &str,
) -> Result<Option<String>, OrbitError> {
    let output = git_output(
        repo_root,
        &[
            "ls-remote",
            "--heads",
            "origin",
            &format!("refs/heads/{branch}"),
        ],
    )?;
    let sha = output.split_whitespace().next().map(ToOwned::to_owned);
    Ok(sha.filter(|value| !value.is_empty()))
}

fn parse_divergence_count(
    value: Option<&str>,
    label: &str,
    base: &str,
    head: &str,
) -> Result<u64, OrbitError> {
    let raw = value.ok_or_else(|| {
        OrbitError::Execution(format!(
            "missing {label} divergence count while comparing '{head}' to '{base}'"
        ))
    })?;
    raw.parse::<u64>().map_err(|error| {
        OrbitError::Execution(format!(
            "invalid {label} divergence count '{raw}' while comparing '{head}' to '{base}': {error}"
        ))
    })
}
