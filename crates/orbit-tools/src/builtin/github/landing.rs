//! Bounded provider evidence for an integration-branch commit, and the `gh`
//! requests that land one pull request at an exact head.
use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use orbit_types::workflow::MergeMessage;

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
/// refused. A squash or merge commit carries Orbit's composed `message` as
/// `commit_title` and `commit_message`, never the forge's default; a rebase
/// writes no new commit message.
pub fn merge_pull_request_at_head(
    number: u64,
    head_sha: &str,
    method: &str,
    message: &MergeMessage,
) -> ExecRequest {
    let mut args = vec![
        "api".into(),
        format!("repos/{{owner}}/{{repo}}/pulls/{number}/merge"),
        "--method".into(),
        "PUT".into(),
        "-f".into(),
        format!("sha={head_sha}"),
        "-f".into(),
        format!("merge_method={method}"),
    ];
    if method != "rebase" {
        // `-f` sends the raw string: a leading `@` is never read as a file.
        args.extend([
            "-f".into(),
            format!("commit_title={}", message.title),
            "-f".into(),
            format!("commit_message={}", message.body),
        ]);
    }
    super::gh_exec_request(args, None, TIMEOUT_SLOW_MS)
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
