//! The `gh` requests that close a task's Orbit-authored pull requests once the
//! task reaches a terminal decision. Core decides which PRs qualify; this
//! module only owns what each invocation looks like.
use orbit_exec::ExecRequest;

use crate::{TIMEOUT_DEFAULT_MS, TIMEOUT_SLOW_MS};

/// Fields read for each open PR: enough to match its head branch, its body and
/// its author against the task without a second lookup.
const OPEN_PR_FIELDS: &str = "number,headRefName,body,author";

/// Upper bound on open PRs read per lookup. `gh` paginates up to it.
const OPEN_PR_LIMIT: &str = "500";

/// Every open pull request of the repository `gh` resolves from its working
/// directory.
pub fn open_pull_requests() -> ExecRequest {
    super::gh_exec_request(
        vec![
            "pr".into(),
            "list".into(),
            "--state".into(),
            "open".into(),
            "--limit".into(),
            OPEN_PR_LIMIT.into(),
            "--json".into(),
            OPEN_PR_FIELDS.into(),
        ],
        None,
        TIMEOUT_DEFAULT_MS,
    )
}

/// The login `gh` is authenticated as.
pub fn authenticated_login() -> ExecRequest {
    super::gh_exec_request(
        vec!["api".into(), "user".into(), "--jq".into(), ".login".into()],
        None,
        TIMEOUT_DEFAULT_MS,
    )
}

/// Close one pull request with a comment. The head branch is kept: this
/// request never passes `--delete-branch`.
pub fn close_pull_request(number: u64, comment: &str) -> ExecRequest {
    super::gh_exec_request(
        vec![
            "pr".into(),
            "close".into(),
            number.to_string(),
            "--comment".into(),
            comment.into(),
        ],
        None,
        TIMEOUT_SLOW_MS,
    )
}
