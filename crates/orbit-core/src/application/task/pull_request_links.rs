//! Links for `github-pr` refs recorded without one.
//!
//! Delivery records a pull request's page when the provider reports it, but
//! refs written before that, or by a path that knew only the number, carry no
//! URL. A reader fills the link from the workspace's own `origin` remote and
//! the numeric id, never from anything an agent wrote into the task.

use std::cell::OnceCell;
use std::path::PathBuf;

use orbit_common::fs::git::run_git;
use orbit_types::task::{ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM};

use crate::OrbitRuntime;

/// Fills missing `github-pr` links for one read. The remote is read at most
/// once, and only when a ref actually lacks a link.
pub struct PullRequestLinks {
    repo_root: PathBuf,
    repository: OnceCell<Option<String>>,
}

impl PullRequestLinks {
    pub fn new(runtime: &OrbitRuntime) -> Self {
        Self {
            repo_root: runtime.paths().repo_root.clone(),
            repository: OnceCell::new(),
        }
    }

    /// Give each numeric `github-pr` ref without a URL the pull request page in
    /// the workspace's GitHub repository. A workspace whose origin is not on
    /// GitHub, or cannot be read, leaves the refs as they are.
    pub fn link(&self, refs: &mut [ExternalRef]) {
        for external_ref in refs.iter_mut().filter(|external_ref| {
            external_ref.system == GITHUB_PR_EXTERNAL_REF_SYSTEM
                && external_ref.url.is_none()
                && is_pull_request_number(&external_ref.id)
        }) {
            let Some(repository) = self
                .repository
                .get_or_init(|| origin_repository_url(&self.repo_root))
            else {
                return;
            };
            external_ref.url = Some(format!("{repository}/pull/{}", external_ref.id));
        }
    }
}

fn is_pull_request_number(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())
}

fn origin_repository_url(repo_root: &std::path::Path) -> Option<String> {
    let output = run_git(repo_root, &["remote", "get-url", "origin"])
        .inspect_err(|error| {
            tracing::debug!(%error, "pull request links: origin remote unreadable");
        })
        .ok()?;
    if !output.success {
        return None;
    }
    github_repository_url(&output.stdout)
}

/// `https://github.com/<owner>/<repo>` for a GitHub remote in URL or scp form
/// (`https://github.com/o/r.git`, `ssh://git@github.com/o/r`,
/// `git@github.com:o/r.git`); `None` for any other host or shape.
fn github_repository_url(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let (authority, path) = match remote.split_once("://") {
        Some((scheme, rest)) => {
            if !matches!(scheme, "https" | "http" | "ssh" | "git" | "git+ssh") {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            (host.split_once(':').map_or(host, |(host, _)| host), path)
        }
        None => {
            let (authority, path) = remote.split_once(':')?;
            (
                authority
                    .rsplit_once('@')
                    .map_or(authority, |(_, host)| host),
                path,
            )
        }
    };
    if !authority.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (is_path_segment(owner) && is_path_segment(name))
        .then(|| format!("https://github.com/{owner}/{name}"))
}

fn is_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}
