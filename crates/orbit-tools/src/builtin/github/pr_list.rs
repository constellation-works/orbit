use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::{TIMEOUT_DEFAULT_MS, check_exec_result};

/// The `gh pr list --json` fields this tool projects.
const PR_LIST_FIELDS: &str =
    "number,title,state,isDraft,headRefName,headRefOid,baseRefName,url,updatedAt";

const DEFAULT_LIMIT: u64 = 30;
const MAX_LIMIT: u64 = 100;

pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let mut args = vec!["pr".to_string(), "list".to_string()];
    super::push_optional_flag(&mut args, input, "state", "--state")?;
    super::push_optional_flag(&mut args, input, "base", "--base")?;
    super::push_repo_flag(&mut args, input)?;
    args.push("--limit".to_string());
    args.push(super::bounded_limit(input, "limit", DEFAULT_LIMIT, MAX_LIMIT)?.to_string());
    args.push("--json".to_string());
    args.push(PR_LIST_FIELDS.to_string());

    Ok(super::gh_exec_request(args, None, TIMEOUT_DEFAULT_MS))
}

/// `gh pr list` for closed pull requests whose head branch has this name.
///
/// Engine-private CI discovery uses this when its bounded repository-wide
/// closed-PR page is full, so a candidate branch omitted by the page can still
/// be matched without treating the list's sort order as close time.
pub fn build_closed_head_list_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    build_head_list_request(input, "closed")
}

/// `gh pr list` for open pull requests whose head branch has this name.
pub fn build_open_head_list_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    build_head_list_request(input, "open")
}

fn build_head_list_request(input: &Value, state: &str) -> Result<ExecRequest, OrbitError> {
    super::require_str(input, "head")?;
    let mut args = vec![
        "pr".to_string(),
        "list".to_string(),
        "--state".to_string(),
        state.to_string(),
    ];
    super::push_optional_flag(&mut args, input, "head", "--head")?;
    super::push_repo_flag(&mut args, input)?;
    args.push("--limit".to_string());
    args.push(super::bounded_limit(input, "limit", DEFAULT_LIMIT, MAX_LIMIT)?.to_string());
    args.push("--json".to_string());
    args.push(PR_LIST_FIELDS.to_string());

    Ok(super::gh_exec_request(args, None, TIMEOUT_DEFAULT_MS))
}

/// Reshape one `gh pr list` entry, keeping the pull request's current head SHA
/// under a name that cannot be confused with a run's tested commit.
pub fn project_pull_request(pr: &Value) -> Value {
    json!({
        "number": pr["number"],
        "title": pr["title"],
        "state": pr["state"],
        "draft": pr["isDraft"],
        "head_branch": pr["headRefName"],
        "reported_head_sha": pr["headRefOid"],
        "base_branch": pr["baseRefName"],
        "url": pr["url"],
        "updated_at": pr["updatedAt"],
    })
}

super::gh_tool! {
    pub struct GithubPrListTool;
    name: "github.pr.list";
    description: "List pull requests with their current head SHAs. Defaults to open pull requests.";
    parameters: [
        super::tool_param("state", "Pull-request state filter: open (default), closed, merged, or all", "string", false),
        super::tool_param("base", "Restrict to pull requests targeting one base branch", "string", false),
        super::tool_param("limit", "Maximum pull requests to return (default 30, capped at 100)", "integer", false),
        super::tool_param("repo", "Repository in owner/name format (uses current directory if omitted)", "string", false),
    ];
    request: |ctx, input| {
        build_exec_request(input).map(|request| super::in_tool_workspace(request, ctx))
    }
    response: |_ctx, _input, result| {
        check_exec_result(result, "gh pr list")?;

        let entries = super::parse_gh_json(&result.stdout, "gh pr list")?;
        let pull_requests: Vec<Value> = entries
            .as_array()
            .map(|entries| entries.iter().map(project_pull_request).collect())
            .unwrap_or_default();

        Ok(json!({ "count": pull_requests.len(), "pull_requests": pull_requests }))
    }
}
