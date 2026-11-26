//! Base-branch refresh helpers for [`super::GhTaskPrForge`]:
//! read-only `git merge-tree` conflict detection (naming conflicting paths
//! without touching the working tree or index) and GitHub's single-merge-commit
//! `update-branch` request.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::fs::git::{GitAncestry, git_ancestry};
use orbit_exec::{
    EnvironmentMode, ExecRequest, ExecutionResult, NoSandbox, StdinMode, run_process,
};
use orbit_types::plugin::is_full_commit_id;
use serde_json::Value;

use super::{
    FORGE_QUOTA_CALLER, PullRequestRefreshOutcome, forge_refused_request, view_pull_request,
};

/// Result of inspecting whether `base_sha` can be merged cleanly into `head_sha`
/// as one merge commit without rewriting history or resolving conflicts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseMergeInspection {
    /// `head_sha` already contains `base_sha`; no refresh is needed.
    AlreadyUpToDate,
    /// `head_sha` and `base_sha` merge cleanly with no conflicts.
    Clean {
        /// Resulting toplevel tree OID from `git merge-tree --write-tree`.
        merged_tree: String,
    },
    /// Merging `base_sha` into `head_sha` would conflict on these paths.
    Conflicting {
        /// Sorted, deduplicated repository-relative conflicting paths.
        conflicting_paths: Vec<String>,
    },
    /// `head_sha` and `base_sha` share no common ancestor; updating would
    /// require rewriting published commits.
    UnrelatedHistory,
}

const GIT_TIMEOUT_MS: u64 = 30_000;
const REFRESH_HEAD_POLL_BACKOFF_SECS: [u64; 5] = [1, 2, 3, 4, 5];

/// Inspect a merge of `base_sha` into `head_sha` purely in the Git object
/// store, without modifying the working tree, index or any ref.
pub fn inspect_base_merge(
    env: &orbit_common::Env,
    repo_root: &Path,
    head_sha: &str,
    base_sha: &str,
) -> Result<BaseMergeInspection, OrbitError> {
    if !is_full_commit_id(head_sha) || !is_full_commit_id(base_sha) {
        return Err(OrbitError::InvalidInput(
            "inspecting a base merge requires full commit SHAs".to_string(),
        ));
    }
    match git_ancestry(env, repo_root, base_sha, head_sha)? {
        GitAncestry::Ancestor => {
            return Ok(BaseMergeInspection::AlreadyUpToDate);
        }
        GitAncestry::NotAncestor => {}
        GitAncestry::Unanswered { stderr, .. } => {
            return Err(OrbitError::Execution(format!(
                "git merge-base --is-ancestor failed: {}",
                stderr.trim()
            )));
        }
    }
    let common = run_git(
        env,
        repo_root,
        &["merge-base", "--end-of-options", head_sha, base_sha],
    )?;
    if !common.success {
        if common.exit_code == Some(1) {
            return Ok(BaseMergeInspection::UnrelatedHistory);
        }
        return Err(OrbitError::Execution(format!(
            "git merge-base failed: {}",
            common.stderr.trim()
        )));
    }
    let merged = run_git(
        env,
        repo_root,
        &[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "-z",
            "--no-messages",
            "--end-of-options",
            head_sha,
            base_sha,
        ],
    )?;
    if merged.success {
        let tree = merged
            .stdout
            .split('\0')
            .next()
            .map(str::trim)
            .filter(|oid| !oid.is_empty())
            .ok_or_else(|| {
                OrbitError::Execution("git merge-tree returned no tree OID".to_string())
            })?;
        return Ok(BaseMergeInspection::Clean {
            merged_tree: tree.to_string(),
        });
    }
    if merged.exit_code == Some(1) {
        let conflicting_paths = parse_merge_tree_conflicting_paths(&merged.stdout);
        return Ok(BaseMergeInspection::Conflicting { conflicting_paths });
    }
    Err(OrbitError::Execution(format!(
        "git merge-tree failed: {}",
        merged.stderr.trim()
    )))
}

/// Parse the NUL-delimited output of
/// `git merge-tree --write-tree --name-only -z --no-messages` on conflict:
/// `<tree-oid>\0<path1>\0<path2>\0...`.
fn parse_merge_tree_conflicting_paths(stdout: &str) -> Vec<String> {
    stdout
        .split('\0')
        .skip(1)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub(super) fn update_pull_request_branch_with_gh(
    env: &orbit_common::Env,
    repo_root: &Path,
    number: u64,
    head_branch: &str,
    base_branch: &str,
    expected_head_sha: &str,
) -> Result<PullRequestRefreshOutcome, OrbitError> {
    if !is_full_commit_id(expected_head_sha) {
        return Err(OrbitError::InvalidInput(format!(
            "refreshing pull request #{number} requires its exact head commit"
        )));
    }
    if head_branch.trim().is_empty() || base_branch.trim().is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "refreshing pull request #{number} requires non-empty head and base branch names"
        )));
    }
    // Check remote head lease before fetching/merging so a concurrent move is
    // refused cleanly without modifying anything.
    let head_ref = format!("refs/heads/{head_branch}");
    let ls_remote = run_git(
        env,
        repo_root,
        &["ls-remote", "--heads", "origin", &head_ref],
    )?;
    if !ls_remote.success {
        return Err(OrbitError::Execution(format!(
            "git ls-remote for pull request #{number} branch `{head_branch}` failed: {}",
            ls_remote.stderr.trim()
        )));
    }
    let remote_head = ls_remote.stdout.split_whitespace().next();
    if remote_head != Some(expected_head_sha) {
        return Ok(PullRequestRefreshOutcome::Refused {
            reason: format!(
                "pull request #{number} remote head `{}` no longer matches expected head `{expected_head_sha}`",
                remote_head.unwrap_or("missing")
            ),
        });
    }
    let base_refspec = format!("refs/heads/{base_branch}:refs/remotes/origin/{base_branch}");
    let fetch_base = run_git(
        env,
        repo_root,
        &["fetch", "--no-tags", "--quiet", "origin", &base_refspec],
    )?;
    if !fetch_base.success {
        return Err(OrbitError::Execution(format!(
            "git fetch of base branch `{base_branch}` failed: {}",
            fetch_base.stderr.trim()
        )));
    }
    let base_rev = run_git(
        env,
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("origin/{base_branch}^{{commit}}"),
        ],
    )?;
    if !base_rev.success {
        return Err(OrbitError::Execution(format!(
            "base branch `origin/{base_branch}` could not be resolved: {}",
            base_rev.stderr.trim()
        )));
    }
    let base_sha = base_rev.stdout.trim().to_string();
    let head_present = run_git(
        env,
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{expected_head_sha}^{{commit}}"),
        ],
    )?;
    if !head_present.success {
        let fetch_head = run_git(
            env,
            repo_root,
            &["fetch", "--no-tags", "--quiet", "origin", &head_ref],
        )?;
        if !fetch_head.success {
            return Err(OrbitError::Execution(format!(
                "git fetch of pull request #{number} head `{head_branch}` failed: {}",
                fetch_head.stderr.trim()
            )));
        }
    }
    match inspect_base_merge(env, repo_root, expected_head_sha, &base_sha)? {
        BaseMergeInspection::AlreadyUpToDate => {
            return Ok(PullRequestRefreshOutcome::AlreadyUpToDate {
                head_sha: expected_head_sha.to_string(),
            });
        }
        BaseMergeInspection::UnrelatedHistory => {
            return Ok(PullRequestRefreshOutcome::Refused {
                reason: format!(
                    "pull request #{number} head {expected_head_sha} shares no common ancestor with base `{base_branch}`; cannot update without rewriting published commits"
                ),
            });
        }
        BaseMergeInspection::Conflicting { conflicting_paths } => {
            return Ok(PullRequestRefreshOutcome::Conflicting {
                conflicting_paths,
                reason: format!(
                    "merging `{base_branch}` ({base_sha}) into pull request #{number} head {expected_head_sha} would conflict"
                ),
            });
        }
        BaseMergeInspection::Clean { .. } => {}
    }

    let mut request =
        orbit_tools::github_cli::update_pull_request_branch_request(number, expected_head_sha);
    request.current_dir = Some(repo_root.to_string_lossy().into_owned());
    let result = orbit_tools::github_quota::run_gh_request(env, FORGE_QUOTA_CALLER, &request)?;
    if !result.success {
        let stderr = orbit_common::security::redaction::redact_all_and_home(result.stderr.trim());
        if !result.timed_out && forge_refused_request(&stderr) {
            return Ok(PullRequestRefreshOutcome::Refused { reason: stderr });
        }
        return Err(OrbitError::Execution(format!(
            "gh api update-branch of pull request #{number} failed: {stderr}"
        )));
    }

    // GitHub's update-branch endpoint returns HTTP 202 and creates the merge
    // commit asynchronously; observe the new head SHA before returning.
    for delay_secs in REFRESH_HEAD_POLL_BACKOFF_SECS {
        if let Ok(entry) = view_pull_request(env, repo_root, number)
            && let Some(new_head) = entry
                .get("headRefOid")
                .and_then(Value::as_str)
                .filter(|sha| is_full_commit_id(sha) && *sha != expected_head_sha)
        {
            return Ok(PullRequestRefreshOutcome::Updated {
                head_sha: new_head.to_string(),
            });
        }
        std::thread::sleep(Duration::from_secs(delay_secs));
    }
    if let Ok(entry) = view_pull_request(env, repo_root, number)
        && let Some(new_head) = entry
            .get("headRefOid")
            .and_then(Value::as_str)
            .filter(|sha| is_full_commit_id(sha) && *sha != expected_head_sha)
    {
        return Ok(PullRequestRefreshOutcome::Updated {
            head_sha: new_head.to_string(),
        });
    }
    Err(OrbitError::Execution(format!(
        "gh api update-branch for pull request #{number} was accepted, but the new head commit was not observed yet"
    )))
}

fn run_git(
    env: &orbit_common::Env,
    repo_root: &Path,
    args: &[&str],
) -> Result<ExecutionResult, OrbitError> {
    let request = ExecRequest {
        program: "git".to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        current_dir: Some(repo_root.to_string_lossy().into_owned()),
        timeout_ms: Some(GIT_TIMEOUT_MS),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::Inherit,
        debug: false,
    };
    run_process(env, &request, &NoSandbox)
}
