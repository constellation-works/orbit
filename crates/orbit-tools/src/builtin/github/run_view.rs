use orbit_common::OrbitError;
use orbit_exec::ExecRequest;
use serde_json::{Value, json};

use crate::{TIMEOUT_DEFAULT_MS, check_exec_result};

/// The `gh run view --json` fields this tool projects.
const RUN_VIEW_FIELDS: &str = "databaseId,number,workflowName,displayTitle,status,conclusion,event,headBranch,headSha,createdAt,startedAt,updatedAt,url,jobs";

pub fn build_exec_request(input: &Value) -> Result<ExecRequest, OrbitError> {
    let mut args = vec![
        "run".to_string(),
        "view".to_string(),
        super::require_numeric_id(input, "run")?,
    ];
    super::push_repo_flag(&mut args, input)?;
    args.push("--json".to_string());
    args.push(RUN_VIEW_FIELDS.to_string());

    Ok(super::gh_exec_request(args, None, TIMEOUT_DEFAULT_MS))
}

fn project_step(step: &Value) -> Value {
    json!({
        "number": step["number"],
        "name": step["name"],
        "status": step["status"],
        "conclusion": step["conclusion"],
    })
}

/// The web URL for one job.
///
/// `gh run view --json jobs` reports job IDs but no job URL, and an
/// investigation that cannot link a failing job is much harder to review — so
/// build it from the run URL, which the same payload does carry.
fn job_url(run_url: &Value, job_id: &Value) -> Value {
    match (run_url.as_str(), job_id.as_u64()) {
        (Some(run_url), Some(job_id)) => Value::String(format!("{run_url}/job/{job_id}")),
        _ => Value::Null,
    }
}

fn project_job(job: &Value, run_url: &Value) -> Value {
    let steps: Vec<Value> = job["steps"]
        .as_array()
        .map(|steps| steps.iter().map(project_step).collect())
        .unwrap_or_default();
    json!({
        "job_id": job["databaseId"],
        "name": job["name"],
        "status": job["status"],
        "conclusion": job["conclusion"],
        "started_at": job["startedAt"],
        "completed_at": job["completedAt"],
        "url": job_url(run_url, &job["databaseId"]),
        "steps": steps,
    })
}

/// Whether a job or step landed in a state a remediation task must explain.
///
/// `cancelled` and `timed_out` count: a run that never produced a verdict is
/// not a green run, and treating it as one is how a red pipeline gets reported
/// as clean.
pub(super) fn is_unsuccessful(conclusion: &Value) -> bool {
    matches!(
        conclusion.as_str(),
        Some("failure" | "cancelled" | "timed_out" | "action_required" | "startup_failure")
    )
}

pub fn project_run_view(run: &Value) -> Value {
    let jobs: Vec<Value> = run["jobs"]
        .as_array()
        .map(|jobs| {
            jobs.iter()
                .map(|job| project_job(job, &run["url"]))
                .collect()
        })
        .unwrap_or_default();

    let failed_jobs: Vec<Value> = jobs
        .iter()
        .filter(|job| is_unsuccessful(&job["conclusion"]))
        .map(|job| {
            let failed_steps: Vec<Value> = job["steps"]
                .as_array()
                .map(|steps| {
                    steps
                        .iter()
                        .filter(|step| is_unsuccessful(&step["conclusion"]))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "job_id": job["job_id"],
                "name": job["name"],
                "conclusion": job["conclusion"],
                "url": job["url"],
                "failed_steps": failed_steps,
            })
        })
        .collect();

    json!({
        "run_id": run["databaseId"],
        "run_number": run["number"],
        "workflow": run["workflowName"],
        "title": run["displayTitle"],
        "status": run["status"],
        "conclusion": run["conclusion"],
        "event": run["event"],
        "head_branch": run["headBranch"],
        // Event metadata, not evidence: `github.run.logs` reports the commit
        // the runner actually checked out.
        "reported_head_sha": run["headSha"],
        "created_at": run["createdAt"],
        "started_at": run["startedAt"],
        "updated_at": run["updatedAt"],
        "url": run["url"],
        "jobs": jobs,
        "failed_jobs": failed_jobs,
    })
}

super::gh_tool! {
    pub struct GithubRunViewTool;
    name: "github.run.view";
    description: "Inspect one GitHub Actions workflow run: its reported head SHA, event, branch, and every job with its URL, steps, and conclusions. Unsuccessful jobs and steps are also collected separately.";
    parameters: [
        super::tool_param("run", "Numeric workflow-run ID", "string", true),
        super::tool_param("repo", "Repository in owner/name format (uses current directory if omitted)", "string", false),
    ];
    request: |_ctx, input| {
        build_exec_request(input)
    }
    response: |_ctx, _input, result| {
        check_exec_result(result, "gh run view")?;

        let run = super::parse_gh_json(&result.stdout, "gh run view")?;
        Ok(project_run_view(&run))
    }
}
