//! The `gh` requests that close a task's Orbit-authored pull requests once the
//! task reaches a terminal decision, and that read the merged ones a task
//! settles from. Core decides which PRs qualify; this module only owns what
//! each invocation looks like.
use orbit_exec::ExecRequest;

use crate::{TIMEOUT_DEFAULT_MS, TIMEOUT_SLOW_MS};

/// Fields read for each open PR: enough to match its head branch, its body and
/// its author against the task without a second lookup.
const OPEN_PR_FIELDS: &str = "number,headRefName,body,author";

/// Upper bound on open PRs read per lookup. `gh` paginates up to it.
const OPEN_PR_LIMIT: &str = "500";

/// What a merged PR adds to the match fields: the branch it merged into and
/// the commit it merged as.
const MERGE_FIELDS: &str = "baseRefName,mergeCommit";

/// How many of the most recently merged PRs one lookup reads. A delivery
/// merged further back is settled through its own number instead.
const MERGED_PR_LIMIT: &str = "100";

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

/// The most recently merged pull requests of the repository `gh` resolves
/// from its working directory, with the fields a merged one is matched by.
pub fn merged_pull_requests() -> ExecRequest {
    super::gh_exec_request(
        vec![
            "pr".into(),
            "list".into(),
            "--state".into(),
            "merged".into(),
            "--limit".into(),
            MERGED_PR_LIMIT.into(),
            "--json".into(),
            format!("{OPEN_PR_FIELDS},{MERGE_FIELDS}"),
        ],
        None,
        TIMEOUT_DEFAULT_MS,
    )
}

/// Pull request `number` with its state, the fields an open one is matched
/// by, and the fields a merged one adds.
pub fn view_pull_request(number: u64) -> ExecRequest {
    super::gh_exec_request(
        vec![
            "pr".into(),
            "view".into(),
            number.to_string(),
            "--json".into(),
            format!("state,{OPEN_PR_FIELDS},{MERGE_FIELDS}"),
        ],
        None,
        TIMEOUT_DEFAULT_MS,
    )
}

/// What holds open pull request `number` back from merging, as an
/// out-of-run auto-fix repair reads it: its author, body and head, whether it
/// conflicts with its base, and each status context on its head with whether
/// branch protection requires it. `gh pr view` does not report which checks are
/// required, so this asks the GraphQL API, as completion's watcher does.
pub fn pull_request_blockers(number: u64) -> ExecRequest {
    let query = "query($owner:String!,$repo:String!,$pr:Int!){repository(owner:$owner,name:$repo){\
        pullRequest(number:$pr){state body author{login} headRefName headRefOid mergeable \
        commits(last:1){nodes{commit{oid statusCheckRollup{contexts(first:100){\
        pageInfo{hasNextPage} nodes{__typename \
        ... on CheckRun{name status conclusion isRequired(pullRequestNumber:$pr)} \
        ... on StatusContext{context state isRequired(pullRequestNumber:$pr)}}}}}}}}}}";
    super::gh_exec_request(
        vec![
            "api".into(),
            "graphql".into(),
            "-F".into(),
            "owner={owner}".into(),
            "-F".into(),
            "repo={repo}".into(),
            "-F".into(),
            format!("pr={number}"),
            "-f".into(),
            format!("query={query}"),
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
