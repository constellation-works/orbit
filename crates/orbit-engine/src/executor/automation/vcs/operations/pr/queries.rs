use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::super::DEFAULT_TIMEOUT_MS;
use super::super::input::{optional_string, required_string};
use super::super::process::execute_with_transient_retry;

/// Upper bound on pull requests one listing of every open pull request reads.
const PR_LIST_OPEN_LIMIT: &str = "500";

/// The pull requests on one head branch in `state`.
pub(in crate::executor::automation::vcs::operations) fn pr_list(
    input: &Value,
) -> Result<Value, OrbitError> {
    let head = required_string(input, "head")?;
    let state = optional_string(input, "state").unwrap_or("open");
    list_pull_requests(
        input,
        &[
            "--state",
            state,
            "--head",
            head,
            "--json",
            "number,title,headRefName,author",
        ],
    )
}

/// Every open pull request with its body, which is how a run finds its
/// tasks' earlier delivery pull requests. Bounded by [`PR_LIST_OPEN_LIMIT`].
pub(in crate::executor::automation::vcs::operations) fn pr_list_open(
    input: &Value,
) -> Result<Value, OrbitError> {
    list_pull_requests(
        input,
        &[
            "--state",
            "open",
            "--limit",
            PR_LIST_OPEN_LIMIT,
            "--json",
            "number,title,headRefName,author,body",
        ],
    )
}

fn list_pull_requests(input: &Value, filters: &[&str]) -> Result<Value, OrbitError> {
    let workspace_path = required_string(input, "workspace_path")?;
    let args = ["pr", "list"]
        .iter()
        .chain(filters)
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let result = execute_with_transient_retry(
        "gh",
        &args,
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "PR list",
    )?;
    let pull_requests: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR list returned invalid JSON: {error}"
        ))
    })?;
    if !pull_requests.is_array() {
        return Err(OrbitError::Execution(
            "private automation VCS PR list did not return an array".to_string(),
        ));
    }
    Ok(json!({ "pull_requests": pull_requests }))
}

pub(in crate::executor::automation::vcs::operations) fn pr_view(
    input: &Value,
) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !valid_pr_selector(selector) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        )));
    }
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        selector.to_string(),
        "--json".to_string(),
        "number,title,body,isDraft,headRefName,files,commits,url".to_string(),
    ];
    let result = execute_with_transient_retry(
        "gh",
        &args,
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "PR view",
    )?;
    let pull_request: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR view returned invalid JSON: {error}"
        ))
    })?;
    Ok(json!({ "pull_request": pull_request }))
}

/// Read the repository merge methods and the target branch's linear-history
/// rule in one GraphQL snapshot. Completion resolves a method from this live
/// state before asking GitHub to merge; it never mutates repository settings.
pub(in crate::executor::automation::vcs::operations) fn pr_merge_capabilities(
    input: &Value,
) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let pr_number = pr_number_from_selector(selector).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        ))
    })?;

    let repository = execute_with_transient_retry(
        "gh",
        &[
            "repo".to_string(),
            "view".to_string(),
            "--json".to_string(),
            "nameWithOwner".to_string(),
        ],
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "repository identity",
    )?;
    let repository: Value = serde_json::from_str(&repository.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS repository identity returned invalid JSON: {error}"
        ))
    })?;
    let name_with_owner = repository
        .get("nameWithOwner")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS repository identity omitted nameWithOwner".to_string(),
            )
        })?;
    let (owner, name) = name_with_owner.split_once('/').ok_or_else(|| {
        OrbitError::Execution(format!(
            "private automation VCS repository identity '{name_with_owner}' is not owner/name"
        ))
    })?;

    let query = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){autoMergeAllowed mergeCommitAllowed rebaseMergeAllowed squashMergeAllowed pullRequest(number:$number){baseRefName baseRef{branchProtectionRule{requiresLinearHistory}}}}}";
    let response = execute_with_transient_retry(
        "gh",
        &[
            "api".to_string(),
            "graphql".to_string(),
            "-f".to_string(),
            format!("query={query}"),
            "-F".to_string(),
            format!("owner={owner}"),
            "-F".to_string(),
            format!("name={name}"),
            "-F".to_string(),
            format!("number={pr_number}"),
        ],
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "merge capabilities",
    )?;
    let response: Value = serde_json::from_str(&response.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS merge capabilities returned invalid JSON: {error}"
        ))
    })?;
    normalize_merge_capabilities(&response, name_with_owner)
}

fn normalize_merge_capabilities(
    response: &Value,
    name_with_owner: &str,
) -> Result<Value, OrbitError> {
    let repository = response
        .pointer("/data/repository")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS merge capabilities omitted data.repository".to_string(),
            )
        })?;
    let pull_request = repository
        .get("pullRequest")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS merge capabilities could not resolve the pull request"
                    .to_string(),
            )
        })?;
    let required_bool = |key: &str| {
        repository.get(key).and_then(Value::as_bool).ok_or_else(|| {
            OrbitError::Execution(format!(
                "private automation VCS merge capabilities omitted boolean {key}"
            ))
        })
    };
    let base_branch = pull_request
        .get("baseRefName")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            OrbitError::Execution(
                "private automation VCS merge capabilities omitted the PR base branch".to_string(),
            )
        })?;
    let base_ref = pull_request.get("baseRef").ok_or_else(|| {
        OrbitError::Execution(
            "private automation VCS merge capabilities omitted PR baseRef policy data".to_string(),
        )
    })?;
    if base_ref.is_null() {
        return Err(OrbitError::Execution(
            "private automation VCS merge capabilities could not resolve the PR base ref"
                .to_string(),
        ));
    }
    let branch_protection_rule = base_ref.get("branchProtectionRule").ok_or_else(|| {
        OrbitError::Execution(
            "private automation VCS merge capabilities omitted branchProtectionRule policy data"
                .to_string(),
        )
    })?;
    let requires_linear_history = if branch_protection_rule.is_null() {
        false
    } else {
        branch_protection_rule
            .get("requiresLinearHistory")
            .and_then(Value::as_bool)
            .ok_or_else(|| {
                OrbitError::Execution(
                    "private automation VCS merge capabilities omitted boolean requiresLinearHistory"
                        .to_string(),
                )
            })?
    };

    Ok(json!({
        "repository": {
            "name_with_owner": name_with_owner,
            "base_branch": base_branch,
            "allow_squash_merge": required_bool("squashMergeAllowed")?,
            "allow_rebase_merge": required_bool("rebaseMergeAllowed")?,
            "allow_merge_commit": required_bool("mergeCommitAllowed")?,
            "allow_auto_merge": required_bool("autoMergeAllowed")?,
            "requires_linear_history": requires_linear_history,
        }
    }))
}

/// Read the merge-relevant state of a PR.
///
/// Separate from [`pr_view`], whose field set is fixed to the review-body
/// concerns its callers need. Completion asks a different question — is this PR
/// actually merged, and if not, what is holding it — so it selects the merge
/// state fields instead of widening a shared projection.
pub(in crate::executor::automation::vcs::operations) fn pr_status(
    input: &Value,
) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !valid_pr_selector(selector) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        )));
    }
    let args = vec![
        "pr".to_string(),
        "view".to_string(),
        selector.to_string(),
        "--json".to_string(),
        "number,state,isDraft,mergedAt,mergeable,mergeStateStatus,statusCheckRollup,reviewDecision,headRefName,headRefOid,baseRefName,mergeCommit,url,autoMergeRequest".to_string(),
    ];
    let result = if input.get("deadline_ms").is_some() {
        super::super::super::completion_deadline::Deadline::from_input(input)?.execute(
            "gh",
            args,
            Path::new(workspace_path),
            "PR status",
        )?
    } else {
        execute_with_transient_retry(
            "gh",
            &args,
            Some(Path::new(workspace_path)),
            DEFAULT_TIMEOUT_MS,
            "PR status",
        )?
    };
    let pull_request: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR status returned invalid JSON: {error}"
        ))
    })?;
    Ok(json!({ "pull_request": pull_request }))
}

/// Which checks on a PR's head commit branch protection requires.
///
/// `gh pr view --json statusCheckRollup` does not say, so this asks the
/// GraphQL API for each rollup context's `isRequired` against the PR, with a
/// check run's own and its app's identity. The
/// answer carries the head commit it describes; the reader in `pr/answers.rs`
/// refuses one that is truncated or names another head.
pub(in crate::executor::automation::vcs::operations) fn pr_required_checks(
    input: &Value,
) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let Some(number) = pr_number_from_selector(selector) else {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        )));
    };
    let query = "query($owner:String!,$repo:String!,$pr:Int!){repository(owner:$owner,name:$repo){\
        pullRequest(number:$pr){headRefOid commits(last:1){nodes{commit{oid \
        statusCheckRollup{contexts(first:100){pageInfo{hasNextPage} nodes{__typename \
        ... on CheckRun{databaseId name status conclusion isRequired(pullRequestNumber:$pr) \
        checkSuite{app{databaseId} workflowRun{workflow{name}}}} \
        ... on StatusContext{context state isRequired(pullRequestNumber:$pr)}}}}}}}}}}";
    let args = vec![
        "api".to_string(),
        "graphql".to_string(),
        "-F".to_string(),
        "owner={owner}".to_string(),
        "-F".to_string(),
        "repo={repo}".to_string(),
        "-F".to_string(),
        format!("pr={number}"),
        "-f".to_string(),
        format!("query={query}"),
    ];
    let result = if input.get("deadline_ms").is_some() {
        super::super::super::completion_deadline::Deadline::from_input(input)?.execute(
            "gh",
            args,
            Path::new(workspace_path),
            "PR required checks",
        )?
    } else {
        execute_with_transient_retry(
            "gh",
            &args,
            Some(Path::new(workspace_path)),
            DEFAULT_TIMEOUT_MS,
            "PR required checks",
        )?
    };
    let answer: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR required checks returned invalid JSON: {error}"
        ))
    })?;
    Ok(json!({ "pull_request": answer.pointer("/data/repository/pullRequest").cloned() }))
}

fn valid_pr_selector(value: &str) -> bool {
    pr_number_from_selector(value).is_some()
}

fn pr_number_from_selector(value: &str) -> Option<&str> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(value);
    }
    if !value.contains("github.com/") || !value.contains("/pull/") {
        return None;
    }
    value
        .rsplit('/')
        .next()
        .filter(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}
