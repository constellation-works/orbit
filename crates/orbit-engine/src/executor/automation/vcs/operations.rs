use std::path::Path;

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
use serde_json::{Value, json};

pub(crate) const PUSH: &str = "push";
pub(crate) const PR_LIST: &str = "pr.list";
pub(crate) const PR_CREATE: &str = "pr.create";
pub(crate) const PR_VIEW: &str = "pr.view";
pub(crate) const PR_MERGE: &str = "pr.merge";
pub(crate) const PR_MERGE_CAPABILITIES: &str = "pr.merge_capabilities";
pub(crate) const PR_STATUS: &str = "pr.status";

const DEFAULT_TIMEOUT_MS: u64 = 15_000;
const SLOW_TIMEOUT_MS: u64 = 30_000;
const LONG_TIMEOUT_MS: u64 = 60_000;

/// Execute the VCS operations owned by deterministic shipment automation.
///
/// This boundary is deliberately separate from `ToolRegistry`: the operation
/// labels are engine-private, are never advertised to agents, and do not pass
/// through public tool authorization or activity allowlists.
pub(crate) fn run(operation: &str, input: &Value) -> Result<Value, OrbitError> {
    match operation {
        PUSH => push(input),
        PR_LIST => pr_list(input),
        PR_CREATE => pr_create(input),
        PR_VIEW => pr_view(input),
        PR_MERGE => pr_merge(input),
        PR_MERGE_CAPABILITIES => pr_merge_capabilities(input),
        PR_STATUS => pr_status(input),
        other => Err(OrbitError::InvalidInput(format!(
            "unknown private automation VCS operation '{other}'"
        ))),
    }
}

fn push(input: &Value) -> Result<Value, OrbitError> {
    let repo_root = required_string(input, "repo_root")?;
    let branch = required_string(input, "branch")?;
    let force_with_lease = input
        .get("force_with_lease")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let expected_remote_sha = optional_string(input, "expected_remote_sha");

    reject_option_like("branch", branch)?;
    if force_with_lease && !valid_expected_remote_sha(expected_remote_sha) {
        return Err(OrbitError::InvalidInput(
            "private automation VCS push requires an exact 40- or 64-character expected_remote_sha when force_with_lease is true"
                .to_string(),
        ));
    }

    let mut args = vec!["push".to_string()];
    if let Some(expected_remote_sha) = force_with_lease.then_some(expected_remote_sha).flatten() {
        args.push(format!(
            "--force-with-lease=refs/heads/{branch}:{expected_remote_sha}"
        ));
    }
    args.extend(["--".to_string(), "origin".to_string(), branch.to_string()]);
    let result = execute(
        "git",
        args,
        Some(Path::new(repo_root)),
        LONG_TIMEOUT_MS,
        "push",
    )?;
    Ok(json!({
        "stdout": result.stdout,
        "stderr": result.stderr,
    }))
}

fn pr_list(input: &Value) -> Result<Value, OrbitError> {
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
    let result = execute(
        "gh",
        args,
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

fn pr_create(input: &Value) -> Result<Value, OrbitError> {
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

fn pr_view(input: &Value) -> Result<Value, OrbitError> {
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
    let result = execute(
        "gh",
        args,
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

fn pr_merge(input: &Value) -> Result<Value, OrbitError> {
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
        return pr_merge_reviewed(selector, workspace_path, strategy, reviewed_head);
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
fn pr_merge_reviewed(
    selector: &str,
    workspace_path: &str,
    strategy: &str,
    reviewed_head: &str,
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
    let result = execute(
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
        "reviewed PR merge",
    )?;
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

/// Read the repository merge methods and the target branch's linear-history
/// rule in one GraphQL snapshot. Completion resolves a method from this live
/// state before asking GitHub to merge; it never mutates repository settings.
fn pr_merge_capabilities(input: &Value) -> Result<Value, OrbitError> {
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

pub(super) fn normalize_merge_capabilities(
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
fn pr_status(input: &Value) -> Result<Value, OrbitError> {
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
        "number,state,mergedAt,mergeable,mergeStateStatus,headRefName,headRefOid,baseRefName,mergeCommit,url".to_string(),
    ];
    let result = execute(
        "gh",
        args,
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

fn execute(
    program: &str,
    args: Vec<String>,
    current_dir: Option<&Path>,
    timeout_ms: u64,
    operation: &str,
) -> Result<orbit_exec::ExecutionResult, OrbitError> {
    let request = if program == "git" {
        let root = current_dir.ok_or_else(|| {
            OrbitError::InvalidInput("Git operation requires a working directory".to_string())
        })?;
        let args = args.iter().map(String::as_str).collect::<Vec<_>>();
        super::git::git_request(root, &args, timeout_ms)
    } else {
        ExecRequest {
            program: program.to_string(),
            args,
            current_dir: current_dir.map(|path| path.to_string_lossy().into_owned()),
            timeout_ms: Some(timeout_ms),
            stdin_mode: StdinMode::Null,
            environment_mode: EnvironmentMode::ClearAndSet(super::git::vcs_environment(&[
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_ENTERPRISE_TOKEN",
                "GITHUB_ENTERPRISE_TOKEN",
                "GH_HOST",
                "GH_CONFIG_DIR",
                "XDG_CONFIG_HOME",
            ])),
            debug: false,
        }
    };
    let result = run_process(&request, &NoSandbox)?;
    if !result.success {
        return Err(OrbitError::Execution(format!(
            "private automation VCS {operation} failed: {}",
            result.stderr.trim()
        )));
    }
    Ok(result)
}

fn required_string<'a>(input: &'a Value, key: &str) -> Result<&'a str, OrbitError> {
    optional_string(input, key).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "private automation VCS operation requires non-empty '{key}' metadata"
        ))
    })
}

fn optional_string<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn reject_option_like(label: &str, value: &str) -> Result<(), OrbitError> {
    if value.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS {label} must not start with '-'"
        )));
    }
    Ok(())
}

fn valid_expected_remote_sha(value: Option<&str>) -> bool {
    value.is_some_and(|sha| {
        matches!(sha.len(), 40 | 64) && sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
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
