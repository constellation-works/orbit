//! Bounded provider evidence for an integration-branch commit, and the `gh`
//! requests that land one pull request at an exact head.
use orbit_common::OrbitError;
use orbit_exec::ExecRequest;

use crate::{TIMEOUT_DEFAULT_MS, TIMEOUT_SLOW_MS};

/// The merge methods the repository checked out in the working directory permits.
pub fn repository_merge_methods() -> ExecRequest {
    super::gh_exec_request(
        vec![
            "repo".into(),
            "view".into(),
            "--json".into(),
            "squashMergeAllowed,rebaseMergeAllowed,mergeCommitAllowed".into(),
        ],
        None,
        TIMEOUT_DEFAULT_MS,
    )
}

/// Merge pull request `number` synchronously at exactly `head_sha`. The REST
/// mutation keeps the forge's branch protection in charge: it never bypasses
/// a rule, enables auto-merge or enters a merge queue, and a moved head is
/// refused.
pub fn merge_pull_request_at_head(number: u64, head_sha: &str, method: &str) -> ExecRequest {
    super::gh_exec_request(
        vec![
            "api".into(),
            format!("repos/{{owner}}/{{repo}}/pulls/{number}/merge"),
            "--method".into(),
            "PUT".into(),
            "-f".into(),
            format!("sha={head_sha}"),
            "-f".into(),
            format!("merge_method={method}"),
        ],
        None,
        TIMEOUT_SLOW_MS,
    )
}

/// Resolve PR identities associated by GitHub with an exact landed commit.
pub fn commit_pull_requests(repository: &str, commit: &str) -> Result<ExecRequest, OrbitError> {
    if !super::is_repository_path(repository)
        || !matches!(commit.len(), 40 | 64)
        || !commit.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(OrbitError::InvalidInput(
            "invalid landing repository or commit".into(),
        ));
    }
    Ok(super::gh_exec_request(
        vec![
            "api".into(),
            format!("repos/{repository}/commits/{commit}/pulls?per_page=100"),
        ],
        None,
        2000,
    ))
}
