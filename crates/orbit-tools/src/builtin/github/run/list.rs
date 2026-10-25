use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::{TIMEOUT_DEFAULT_MS, check_exec_result};

/// The `gh run list --json` fields this tool projects.
const RUN_LIST_FIELDS: &str = "databaseId,number,workflowName,displayTitle,status,conclusion,event,headBranch,headSha,createdAt,startedAt,updatedAt,url";

const DEFAULT_LIMIT: u64 = 20;
/// The largest `--limit` this tool passes to `gh run list`; larger requests
/// are clamped to it.
pub const MAX_LIMIT: u64 = 100;

/// `gh run list` stderr when `--workflow` matches more than one workflow.
const AMBIGUOUS_WORKFLOW_MARKER: &str = "could not resolve to a unique workflow";
/// `gh run list` stderr when `--workflow` names no workflow.
const UNKNOWN_WORKFLOW_MARKER: &str = "could not find any workflows named";

/// The `workflow` input the caller asked `gh` to resolve, for error reporting.
///
/// String input is trimmed; numeric input is kept as written because a
/// numeric workflow id is an accepted value. Anything else reports as empty,
/// and the `gh` stderr still carries the miss.
fn requested_workflow(input: &Value) -> String {
    match input.get("workflow") {
        Some(Value::String(value)) => value.trim().to_string(),
        Some(Value::Number(value)) => value.to_string(),
        _ => String::new(),
    }
}

/// The match list `gh` prints after `found:` in its ambiguous-workflow
/// stderr, or `None` when the stderr names no trailing list.
fn ambiguous_workflow_matches(stderr: &str) -> Option<String> {
    let marker = "found:";
    let start = stderr.to_lowercase().find(marker)? + marker.len();
    let matches = stderr[start..].trim().to_string();
    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

/// Map a `gh` workflow-resolution miss to invalid input.
///
/// `gh` exits non-zero both when `--workflow` is ambiguous and when it names
/// no workflow. Both mean the caller did not identify a workflow, so they are
/// refused without choosing a match. Every other `gh` failure keeps the
/// shared execution-error path.
fn workflow_resolution_error(
    input: &Value,
    result: &orbit_types::tool::ExecutionResult,
) -> Option<OrbitError> {
    if result.success {
        return None;
    }
    let stderr = result.stderr.trim();
    if stderr.is_empty() {
        return None;
    }
    let lowered = stderr.to_lowercase();
    let workflow = requested_workflow(input);
    let display = if workflow.is_empty() {
        "unknown".to_string()
    } else {
        workflow
    };
    if lowered.contains(AMBIGUOUS_WORKFLOW_MARKER) {
        let matches = ambiguous_workflow_matches(stderr).unwrap_or_else(|| stderr.to_string());
        return Some(OrbitError::InvalidInput(format!(
            "ambiguous workflow \"{display}\": gh could not resolve it to a unique workflow; found: {matches}. Pass one workflow file name (for example codeql.yml) or a numeric workflow id; a job display name is not a workflow name. Pass one of: {matches}; the tool does not choose one."
        )));
    }
    if lowered.contains(UNKNOWN_WORKFLOW_MARKER) {
        return Some(OrbitError::InvalidInput(format!(
            "unknown workflow \"{display}\": gh {stderr}. Pass one workflow file name (for example codeql.yml) or a numeric workflow id; a job display name is not a workflow name."
        )));
    }
    None
}

pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let mut args = vec!["run".to_string(), "list".to_string()];
    super::super::push_optional_flag(&mut args, input, "branch", "--branch")?;
    super::super::push_optional_flag(&mut args, input, "workflow", "--workflow")?;
    super::super::push_optional_flag(&mut args, input, "status", "--status")?;
    super::super::push_optional_flag(&mut args, input, "event", "--event")?;
    super::super::push_repo_flag(&mut args, input)?;
    args.push("--limit".to_string());
    args.push(super::super::bounded_limit(input, "limit", DEFAULT_LIMIT, MAX_LIMIT)?.to_string());
    args.push("--json".to_string());
    args.push(RUN_LIST_FIELDS.to_string());

    Ok(super::super::gh_exec_request(
        args,
        None,
        TIMEOUT_DEFAULT_MS,
    ))
}

/// Reshape one `gh run list` entry into the tool's own field names.
///
/// `reported_head_sha` is deliberately not called `sha`: it is the SHA the
/// workflow event carried, which is not necessarily the commit the runner
/// tested. Read `github.run.logs` for that.
pub fn project_run(run: &Value) -> Value {
    json!({
        "run_id": run["databaseId"],
        "run_number": run["number"],
        "workflow": run["workflowName"],
        "title": run["displayTitle"],
        "status": run["status"],
        "conclusion": run["conclusion"],
        "event": run["event"],
        "head_branch": run["headBranch"],
        "reported_head_sha": run["headSha"],
        "created_at": run["createdAt"],
        "started_at": run["startedAt"],
        "updated_at": run["updatedAt"],
        "url": run["url"],
    })
}

super::super::gh_tool! {
    pub struct GithubRunListTool;
    name: "github.run.list";
    description: "List recent GitHub Actions workflow runs with each run's reported head SHA. Filter by branch, workflow, status/conclusion, and event.";
    parameters: [
        super::super::tool_param("branch", "Restrict to runs for one branch", "string", false),
        super::super::tool_param("workflow", "Restrict to one workflow file name (for example codeql.yml) or numeric workflow id; a job display name is not a workflow name and is not accepted", "string", false),
        super::super::tool_param("status", "Restrict to one run status or conclusion (queued, in_progress, completed, failure, success, cancelled, timed_out, action_required)", "string", false),
        super::super::tool_param("event", "Restrict to one triggering event (push, pull_request, schedule)", "string", false),
        super::super::tool_param("limit", "Maximum runs to return (default 20, capped at 100)", "integer", false),
        super::super::tool_param("repo", "Repository in owner/name format (uses current directory if omitted)", "string", false),
    ];
    request: |ctx, input| {
        build_exec_request(input).map(|request| super::super::in_tool_workspace(request, ctx))
    }
    response: |_ctx, input, result| {
        if let Some(error) = workflow_resolution_error(input, result) {
            return Err(error);
        }
        check_exec_result(result, "gh run list")?;

        let runs = super::super::parse_gh_json(&result.stdout, "gh run list")?;
        let runs: Vec<Value> = runs
            .as_array()
            .map(|entries| entries.iter().map(project_run).collect())
            .unwrap_or_default();

        Ok(json!({ "count": runs.len(), "runs": runs }))
    }
}
