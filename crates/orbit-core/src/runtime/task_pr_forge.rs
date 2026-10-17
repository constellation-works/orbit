//! The forge a task's terminal decision closes Orbit-authored PRs through.
//!
//! When a task lands, is rejected or is archived, Core closes the delivery
//! and `[BLOCKED]` PRs Orbit opened for it (`application::task::pr_closure`),
//! and an owner drain checks a dead delivery leaf's PR is still open before
//! it adopts the leaf. When an operator merges a task's delivery PR outside
//! Orbit, Core settles the task from that merge
//! (`application::task::external_merge`). Those policies only need a few
//! forge operations, so they reach them through [`TaskPrForge`] rather than
//! the `gh` CLI directly: production uses [`GhTaskPrForge`], and fixtures
//! inject a fake forge. No operation deletes a branch.

use std::path::Path;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_exec::{ExecRequest, NoSandbox, run_process};
use serde_json::Value;

/// One open pull request as the closure policy matches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgePullRequest {
    /// Provider PR number.
    pub number: u64,
    /// Head branch name.
    pub head_branch: String,
    /// PR description.
    pub body: String,
    /// Login of the PR's author.
    pub author: String,
}

/// One merged pull request as external-merge settlement matches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedPullRequest {
    /// The fields an open pull request is matched by.
    pub pull: ForgePullRequest,
    /// The branch it merged into.
    pub base_branch: String,
    /// The commit it merged as.
    pub merge_commit: String,
}

/// Forge operations behind closing a task's PRs on a terminal decision and
/// settling a task from its merged delivery PR.
pub trait TaskPrForge: Send + Sync {
    /// Every open pull request of the repository checked out at `repo_root`.
    fn open_pull_requests(&self, repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError>;

    /// Pull request `number` when it is open, read on its own so a
    /// repository with more open pull requests than one listing returns
    /// still answers. The default reads it from that listing.
    fn open_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<ForgePullRequest>, OrbitError> {
        Ok(self
            .open_pull_requests(repo_root)?
            .into_iter()
            .find(|pull| pull.number == number))
    }

    /// The repository's most recently merged pull requests. The default
    /// reports none: a forge that cannot read merges settles nothing.
    fn merged_pull_requests(
        &self,
        _repo_root: &Path,
    ) -> Result<Vec<MergedPullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    /// Pull request `number` when it is merged, read on its own so a delivery
    /// merged before the latest listing still answers. The default reads it
    /// from that listing.
    fn merged_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        Ok(self
            .merged_pull_requests(repo_root)?
            .into_iter()
            .find(|merged| merged.pull.number == number))
    }

    /// The login this machine's forge credentials act as, which is the
    /// identity Orbit opens its PRs under.
    fn authenticated_login(&self, repo_root: &Path) -> Result<String, OrbitError>;

    /// Close one pull request with `comment`, keeping its head branch.
    fn close_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
        comment: &str,
    ) -> Result<(), OrbitError>;
}

/// The forge for a host-bound runtime. This crate's own unit tests never
/// reach a real forge: a done or rejected fixture task must not touch a
/// repository's PRs from whichever checkout the tests run in.
#[must_use]
pub fn default_task_pr_forge() -> Arc<dyn TaskPrForge> {
    if cfg!(test) {
        Arc::new(NoTaskPrForge)
    } else {
        Arc::new(GhTaskPrForge)
    }
}

/// A forge with no PRs, for runtimes that are not bound to a repository.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoTaskPrForge;

impl TaskPrForge for NoTaskPrForge {
    fn open_pull_requests(&self, _repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        Ok(Vec::new())
    }

    fn authenticated_login(&self, _repo_root: &Path) -> Result<String, OrbitError> {
        Err(OrbitError::Execution(
            "no forge is bound to this runtime".to_string(),
        ))
    }

    fn close_pull_request(
        &self,
        _repo_root: &Path,
        number: u64,
        _comment: &str,
    ) -> Result<(), OrbitError> {
        Err(OrbitError::Execution(format!(
            "no forge is bound to this runtime to close pull request #{number}"
        )))
    }
}

/// GitHub through the `gh` CLI, run in the workspace checkout so `gh`
/// resolves the repository from its remotes. The argv lives in
/// `orbit_tools::github_cli`.
#[derive(Debug, Default, Clone, Copy)]
pub struct GhTaskPrForge;

impl TaskPrForge for GhTaskPrForge {
    fn open_pull_requests(&self, repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError> {
        let stdout = run_gh(
            orbit_tools::github_cli::open_pull_requests_request(),
            repo_root,
            "gh pr list",
        )?;
        let entries = orbit_tools::github_cli::parse_gh_json(&stdout, "gh pr list")?;
        let entries = entries.as_array().ok_or_else(|| {
            OrbitError::Execution("gh pr list did not return an array".to_string())
        })?;
        Ok(entries.iter().filter_map(pull_request_from_json).collect())
    }

    fn open_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<ForgePullRequest>, OrbitError> {
        let entry = view_pull_request(repo_root, number)?;
        if !has_state(&entry, "open") {
            return Ok(None);
        }
        pull_request_from_json(&entry).map(Some).ok_or_else(|| {
            OrbitError::Execution(format!(
                "gh pr view #{number} did not report its number, head branch and author"
            ))
        })
    }

    fn merged_pull_requests(&self, repo_root: &Path) -> Result<Vec<MergedPullRequest>, OrbitError> {
        let stdout = run_gh(
            orbit_tools::github_cli::merged_pull_requests_request(),
            repo_root,
            "gh pr list",
        )?;
        let entries = orbit_tools::github_cli::parse_gh_json(&stdout, "gh pr list")?;
        let entries = entries.as_array().ok_or_else(|| {
            OrbitError::Execution("gh pr list did not return an array".to_string())
        })?;
        Ok(entries
            .iter()
            .filter_map(merged_pull_request_from_json)
            .collect())
    }

    fn merged_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
    ) -> Result<Option<MergedPullRequest>, OrbitError> {
        let entry = view_pull_request(repo_root, number)?;
        if !has_state(&entry, "merged") {
            return Ok(None);
        }
        merged_pull_request_from_json(&entry)
            .map(Some)
            .ok_or_else(|| {
                OrbitError::Execution(format!(
                    "gh pr view #{number} did not report its head branch, author, base branch \
                     and merge commit"
                ))
            })
    }

    fn authenticated_login(&self, repo_root: &Path) -> Result<String, OrbitError> {
        let stdout = run_gh(
            orbit_tools::github_cli::authenticated_login_request(),
            repo_root,
            "gh api user",
        )?;
        let login = stdout.trim();
        if login.is_empty() {
            return Err(OrbitError::Execution(
                "gh api user returned no login".to_string(),
            ));
        }
        Ok(login.to_string())
    }

    fn close_pull_request(
        &self,
        repo_root: &Path,
        number: u64,
        comment: &str,
    ) -> Result<(), OrbitError> {
        run_gh(
            orbit_tools::github_cli::close_pull_request_request(number, comment),
            repo_root,
            "gh pr close",
        )
        .map(|_| ())
    }
}

fn view_pull_request(repo_root: &Path, number: u64) -> Result<Value, OrbitError> {
    let stdout = run_gh(
        orbit_tools::github_cli::view_pull_request_request(number),
        repo_root,
        "gh pr view",
    )?;
    orbit_tools::github_cli::parse_gh_json(&stdout, "gh pr view")
}

fn has_state(entry: &Value, state: &str) -> bool {
    entry
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|reported| reported.eq_ignore_ascii_case(state))
}

fn run_gh(mut request: ExecRequest, repo_root: &Path, label: &str) -> Result<String, OrbitError> {
    request.current_dir = Some(repo_root.to_string_lossy().into_owned());
    let result = run_process(&request, &NoSandbox)?;
    if !result.success {
        return Err(OrbitError::Execution(format!(
            "{label} failed: {}",
            result.stderr.trim()
        )));
    }
    Ok(result.stdout)
}

/// An entry missing a number, head branch or author cannot be matched safely,
/// so it is skipped rather than guessed at.
fn pull_request_from_json(entry: &Value) -> Option<ForgePullRequest> {
    Some(ForgePullRequest {
        number: entry.get("number")?.as_u64()?,
        head_branch: entry.get("headRefName")?.as_str()?.to_string(),
        body: entry
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        author: entry.pointer("/author/login")?.as_str()?.to_string(),
    })
}

/// A merged entry also needs the branch it merged into and its merge commit;
/// without them it cannot prove where the task's work landed.
fn merged_pull_request_from_json(entry: &Value) -> Option<MergedPullRequest> {
    let non_empty = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some(MergedPullRequest {
        pull: pull_request_from_json(entry)?,
        base_branch: non_empty(entry.get("baseRefName"))?,
        merge_commit: non_empty(entry.pointer("/mergeCommit/oid"))?,
    })
}
