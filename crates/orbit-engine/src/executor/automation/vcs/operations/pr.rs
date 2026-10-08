use std::path::Path;

use orbit_common::OrbitError;
use serde_json::{Value, json};

use super::input::{optional_string, required_string, valid_expected_remote_sha};
use super::process::{execute, execute_with_transient_retry, run_vcs_process, succeeded};
use super::{BASE_MODIFIED_REFUSAL, DEFAULT_TIMEOUT_MS, SLOW_TIMEOUT_MS};

/// GitHub's reason for refusing a merge because another merge moved the base
/// between its mergeability check and the mutation (F2026-10-071). The `sha`
/// condition was not what failed.
const BASE_MODIFIED_REASON: &str = "Base branch was modified. Review and try the merge again.";

pub(super) fn pr_list(input: &Value) -> Result<Value, OrbitError> {
    let head = required_string(input, "head")?;
    let state = optional_string(input, "state").unwrap_or("open");
    let workspace_path = required_string(input, "workspace_path")?;
    let args = vec![
        "pr".to_string(),
        "list".to_string(),
        "--state".to_string(),
        state.to_string(),
        "--head".to_string(),
        head.to_string(),
        "--json".to_string(),
        "number,title,headRefName,author".to_string(),
    ];
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

pub(super) fn pr_create(input: &Value) -> Result<Value, OrbitError> {
    let title = required_string(input, "title")?;
    let body = required_string(input, "body")?;
    let base = required_string(input, "base")?;
    let head = required_string(input, "head")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let args = vec![
        "pr".to_string(),
        "create".to_string(),
        "--title".to_string(),
        title.to_string(),
        "--body".to_string(),
        body.to_string(),
        "--base".to_string(),
        base.to_string(),
        "--head".to_string(),
        head.to_string(),
    ];
    let result = execute(
        "gh",
        args,
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
        "PR create",
    )?;
    Ok(json!({
        "url": result.stdout.trim(),
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

pub(super) fn pr_view(input: &Value) -> Result<Value, OrbitError> {
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
        "number,title,body,headRefName,files,commits,url".to_string(),
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

pub(super) fn pr_merge(input: &Value) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let strategy = optional_string(input, "strategy").unwrap_or("squash");
    let strategy_flag = match strategy {
        "squash" => "--squash",
        "merge" => "--merge",
        "rebase" => "--rebase",
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "invalid private automation VCS merge strategy '{other}'"
            )));
        }
    };
    // `--auto` queues GitHub's own auto-merge, which still waits for every
    // required check and branch protection. There is deliberately no
    // administrative bypass (`--admin`) in this surface.
    let auto = input.get("auto").and_then(Value::as_bool).unwrap_or(false);
    if let Some(reviewed_head) = optional_string(input, "reviewed_head_sha") {
        if auto {
            return Err(OrbitError::InvalidInput(
                "review_gate_stale: deferred auto-merge cannot guarantee the reviewed head; wait for checks and request a synchronous merge".to_string(),
            ));
        }
        let report_base_modified = input
            .get("report_base_modified")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        return pr_merge_reviewed(
            selector,
            workspace_path,
            strategy,
            reviewed_head,
            report_base_modified,
        );
    }
    let mut args = vec![
        "pr".to_string(),
        "merge".to_string(),
        selector.to_string(),
        strategy_flag.to_string(),
    ];
    if auto {
        args.push("--auto".to_string());
    }
    let result = execute(
        "gh",
        args,
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
        "PR merge",
    )?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

/// Use the synchronous REST mutation: `sha` is checked by GitHub when it
/// merges, and this endpoint never enables auto-merge or enters a merge queue.
/// `gh pr merge --match-head-commit` alone is insufficient because the CLI
/// can choose deferred semantics for a queue-required branch.
///
/// [ORB-14205] With `report_base_modified`, the provider's base-modification
/// refusal returns `{"merged": false, "refusal": "base_modified"}` with the
/// raw output, so the caller can decide whether to ask again. Every other
/// failure, and that one without the opt-in, stays an error.
fn pr_merge_reviewed(
    selector: &str,
    workspace_path: &str,
    strategy: &str,
    reviewed_head: &str,
    report_base_modified: bool,
) -> Result<Value, OrbitError> {
    // Managed completion supplies a number in the current repository. Refuse
    // other selectors here rather than resolving a URL into a different repo.
    if selector.is_empty() || !selector.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(
            "reviewed PR merge requires a pull request number in the workspace repository".into(),
        ));
    }
    if !valid_expected_remote_sha(Some(reviewed_head)) {
        return Err(OrbitError::InvalidInput(
            "reviewed PR merge requires an exact 40- or 64-character reviewed_head_sha".into(),
        ));
    }
    let result = run_vcs_process(
        "gh",
        vec![
            "api".to_string(),
            format!("repos/{{owner}}/{{repo}}/pulls/{selector}/merge"),
            "--method".to_string(),
            "PUT".to_string(),
            "-f".to_string(),
            format!("sha={reviewed_head}"),
            "-f".to_string(),
            format!("merge_method={strategy}"),
        ],
        Some(Path::new(workspace_path)),
        SLOW_TIMEOUT_MS,
    )?;
    if report_base_modified && is_base_modified_refusal(&result) {
        return Ok(json!({
            "merged": false,
            "refusal": BASE_MODIFIED_REFUSAL,
            "stdout": result.stdout,
            "stderr": result.stderr,
        }));
    }
    let result = succeeded(result, "reviewed PR merge")?;
    let response: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!("reviewed PR merge returned invalid JSON: {error}"))
    })?;
    if response.get("merged").and_then(Value::as_bool) != Some(true) {
        return Err(OrbitError::Execution(
            "reviewed PR merge did not confirm a synchronous merge; deferred merges are unsupported".into(),
        ));
    }
    let landed_commit = required_string(&response, "sha")?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
        "landed_commit": landed_commit,
    }))
}

/// True only for a completed `gh api` failure whose whole diagnostic is the
/// provider's base-modification refusal with HTTP 405: stderr is exactly
/// `gh: <reason> (HTTP 405)` and the response body, when present, is a JSON
/// error naming the same reason and status. Other 405s (policy, queue,
/// protection), other statuses, timeouts, and anything ambiguous do not match.
fn is_base_modified_refusal(result: &orbit_exec::ExecutionResult) -> bool {
    if result.success
        || result.timed_out
        || result.stderr.trim() != format!("gh: {BASE_MODIFIED_REASON} (HTTP 405)")
    {
        return false;
    }
    let body = result.stdout.trim();
    if body.is_empty() {
        return true;
    }
    let Ok(Value::Object(body)) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    body.get("message").and_then(Value::as_str) == Some(BASE_MODIFIED_REASON)
        && body
            .get("status")
            .is_none_or(|status| status.as_str() == Some("405") || status.as_u64() == Some(405))
}

/// Read the repository merge methods and the target branch's linear-history
/// rule in one GraphQL snapshot. Completion resolves a method from this live
/// state before asking GitHub to merge; it never mutates repository settings.
pub(super) fn pr_merge_capabilities(input: &Value) -> Result<Value, OrbitError> {
    let selector = required_string(input, "pr")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let pr_number = pr_number_from_selector(selector).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "invalid private automation VCS PR selector '{selector}'; expected a number or GitHub PR URL"
        ))
    })?;

    let repository = execute(
        "gh",
        vec![
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
    let response = execute(
        "gh",
        vec![
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
pub(super) fn pr_status(input: &Value) -> Result<Value, OrbitError> {
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
        "number,state,mergedAt,mergeable,mergeStateStatus,statusCheckRollup,reviewDecision,headRefName,headRefOid,baseRefName,mergeCommit,url".to_string(),
    ];
    let result = execute_with_transient_retry(
        "gh",
        &args,
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "PR status",
    )?;
    let pull_request: Value = serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS PR status returned invalid JSON: {error}"
        ))
    })?;
    Ok(json!({ "pull_request": pull_request }))
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
