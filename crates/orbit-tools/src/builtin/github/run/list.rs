use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::{TIMEOUT_DEFAULT_MS, check_exec_result};

/// The `gh run list --json` fields this tool projects.
const RUN_LIST_FIELDS: &str = "databaseId,number,workflowName,displayTitle,status,conclusion,event,headBranch,headSha,createdAt,startedAt,updatedAt,url";

const DEFAULT_LIMIT: u64 = 20;
const MAX_LIMIT: u64 = 100;

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
        super::super::tool_param("workflow", "Restrict to one workflow by name, file name, or ID", "string", false),
        super::super::tool_param("status", "Restrict to one run status or conclusion (queued, in_progress, completed, failure, success, cancelled, timed_out, action_required)", "string", false),
        super::super::tool_param("event", "Restrict to one triggering event (push, pull_request, schedule)", "string", false),
        super::super::tool_param("limit", "Maximum runs to return (default 20, capped at 100)", "integer", false),
        super::super::tool_param("repo", "Repository in owner/name format (uses current directory if omitted)", "string", false),
    ];
    request: |ctx, input| {
        build_exec_request(input).map(|request| super::super::in_tool_workspace(request, ctx))
    }
    response: |_ctx, _input, result| {
        check_exec_result(result, "gh run list")?;

        let runs = super::super::parse_gh_json(&result.stdout, "gh run list")?;
        let runs: Vec<Value> = runs
            .as_array()
            .map(|entries| entries.iter().map(project_run).collect())
            .unwrap_or_default();

        Ok(json!({ "count": runs.len(), "runs": runs }))
    }
}
