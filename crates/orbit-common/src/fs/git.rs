use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::OrbitError;

use super::io::with_exclusive_file_lock;

/// Lock file inside the git common directory that serializes Orbit-owned
/// updates of remote-tracking refs.
///
/// ORB-11269/ORB-11256 locked only task-pilot (`orbit-task-pilot-fetch`).
/// Delivery `fetch_remote_base` was a second writer of the same
/// `refs/remotes/origin/*` refs, so a linked worktree fetch and
/// `prepare_task_pilot` could CAS-fail each other. One common-dir lock
/// covers every Orbit fetch; do not add a second pilot-only lock.
pub const GIT_FETCH_LOCK_NAME: &str = "orbit-git-fetch";

/// Bounded attempts for a single Orbit-owned fetch, including the first try.
pub const GIT_FETCH_CAS_ATTEMPTS: u32 = 3;

const GIT_FETCH_CAS_RETRY_DELAY: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CurrentBranchStatus {
    Named(String),
    DetachedHead,
    NoCurrentBranch,
}

pub fn current_branch(workspace_path: &Path) -> Result<CurrentBranchStatus, OrbitError> {
    let symbolic = run_git(
        workspace_path,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
    )?;
    if symbolic.success {
        let branch = symbolic.stdout.trim();
        if branch.is_empty() {
            return Ok(CurrentBranchStatus::NoCurrentBranch);
        }
        return Ok(CurrentBranchStatus::Named(branch.to_string()));
    }

    let verify_head = run_git(workspace_path, &["rev-parse", "--verify", "-q", "HEAD"])?;
    if verify_head.success {
        return Ok(CurrentBranchStatus::DetachedHead);
    }

    Ok(CurrentBranchStatus::NoCurrentBranch)
}

pub fn default_branch(workspace_path: &Path) -> Result<Option<String>, OrbitError> {
    for remote in preferred_remotes(workspace_path)? {
        if let Some(branch) = remote_default_branch(workspace_path, &remote)? {
            return Ok(Some(branch));
        }
    }

    let local_branches = local_branches(workspace_path)?;
    for branch in ["main", "master", "trunk", "develop", "development", "dev"] {
        if local_branches.iter().any(|candidate| candidate == branch) {
            return Ok(Some(branch.to_string()));
        }
    }

    if local_branches.len() == 1 {
        return Ok(local_branches.into_iter().next());
    }

    Ok(None)
}

fn preferred_remotes(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let remotes = run_git(workspace_path, &["remote"])?;
    if !remotes.success {
        return Ok(Vec::new());
    }

    let mut names: Vec<String> = remotes
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    names.sort();
    if let Some(origin_index) = names.iter().position(|name| name == "origin") {
        let origin = names.remove(origin_index);
        names.insert(0, origin);
    }
    Ok(names)
}

fn remote_default_branch(
    workspace_path: &Path,
    remote: &str,
) -> Result<Option<String>, OrbitError> {
    let remote_head = run_git(
        workspace_path,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            &format!("refs/remotes/{remote}/HEAD"),
        ],
    )?;
    if !remote_head.success {
        return Ok(None);
    }

    let head = remote_head.stdout.trim();
    if let Some(branch) = head.strip_prefix(&format!("{remote}/")) {
        return Ok(Some(branch.to_string()));
    }
    if !head.is_empty() {
        return Ok(Some(head.to_string()));
    }
    Ok(None)
}

fn local_branches(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    let branches = run_git(
        workspace_path,
        &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
    )?;
    if !branches.success {
        return Ok(Vec::new());
    }

    Ok(branches
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

pub fn git_common_dir(workspace_path: &Path) -> Result<PathBuf, OrbitError> {
    let output = run_git(
        workspace_path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    if !output.success {
        return Err(OrbitError::Execution(format!(
            "unable to locate shared Git directory in '{}': {}",
            workspace_path.display(),
            output.stderr.trim()
        )));
    }
    let raw = output.stdout.trim();
    if raw.is_empty() {
        return Err(OrbitError::Execution(format!(
            "unable to locate shared Git directory in '{}'",
            workspace_path.display()
        )));
    }
    Ok(PathBuf::from(raw))
}

pub fn git_fetch_lock_target(git_common_dir: &Path) -> PathBuf {
    git_common_dir.join(GIT_FETCH_LOCK_NAME)
}

/// Run `op` while holding the exclusive lock that serializes Orbit-owned
/// remote-tracking-ref updates across linked checkouts.
pub fn with_git_fetch_lock<T, E, F>(workspace: &Path, op: F) -> Result<T, E>
where
    F: FnOnce() -> Result<T, E>,
    E: From<io::Error>,
{
    let common = git_common_dir(workspace).map_err(|error| io::Error::other(error.to_string()))?;
    with_exclusive_file_lock(&git_fetch_lock_target(&common), "git fetch", op)
}

/// True when git refused a ref update because another process raced the same
/// remote-tracking ref. Auth and network failures stay false so callers do
/// not retry them.
pub fn is_git_ref_update_contention(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    if looks_like_remote_auth_or_network_failure(&text) {
        return false;
    }
    text.contains("cannot lock ref") || text.contains("unable to update local ref")
}

pub fn should_retry_git_ref_cas(attempt: u32, stderr: &str) -> bool {
    attempt + 1 < GIT_FETCH_CAS_ATTEMPTS && is_git_ref_update_contention(stderr)
}

pub fn git_fetch_cas_retry_delay() -> Duration {
    GIT_FETCH_CAS_RETRY_DELAY
}

fn looks_like_remote_auth_or_network_failure(stderr_lower: &str) -> bool {
    stderr_lower.contains("authentication failed")
        || stderr_lower.contains("could not resolve host")
        || stderr_lower.contains("unable to access")
        || stderr_lower.contains("terminal prompts disabled")
        || stderr_lower.contains("could not read username")
        || stderr_lower.contains("permission denied (publickey)")
        || stderr_lower.contains("the requested url returned error: 401")
        || stderr_lower.contains("the requested url returned error: 403")
}

pub fn run_git(workspace_path: &Path, args: &[&str]) -> Result<GitCommandOutput, OrbitError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace_path)
        .output()
        .map_err(|error| {
            OrbitError::Execution(format!(
                "failed to run `git {}` in '{}': {error}",
                args.join(" "),
                workspace_path.display()
            ))
        })?;

    Ok(GitCommandOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub struct GitCommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}
