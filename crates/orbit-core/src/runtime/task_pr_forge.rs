//! The forge a task's terminal decision closes Orbit-authored PRs through.
//!
//! When a task lands, is rejected or is archived, Core closes the delivery
//! and `[BLOCKED]` PRs Orbit opened for it (`application::task::pr_closure`).
//! That policy only needs three forge operations, so it reaches them through
//! [`TaskPrForge`] rather than the `gh` CLI directly: production uses
//! [`GhTaskPrForge`], and fixtures inject a fake forge. No operation deletes a
//! branch.

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

/// Forge operations behind closing a task's PRs on a terminal decision.
pub trait TaskPrForge: Send + Sync {
    /// Every open pull request of the repository checked out at `repo_root`.
    fn open_pull_requests(&self, repo_root: &Path) -> Result<Vec<ForgePullRequest>, OrbitError>;

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
