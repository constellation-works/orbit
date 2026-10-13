//! [ORB-15956] The hosted-CI reads and the one rerun the owner's landing makes
//! when a pull request's check fails: which jobs ran on a commit, a failed
//! job's bounded log, and a rerun of a run's failed jobs.
//!
//! The log read is the CI sweep's own (`orbit_tools::github_cli`), so landing
//! classifies a failure from exactly the excerpt the sweep would file.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::github_cli;
use serde_json::{Value, json};

use super::DEFAULT_TIMEOUT_MS;
use super::input::{required_string, valid_expected_remote_sha};
use super::process::{execute, execute_with_transient_retry};

/// Largest failed-job log excerpt one read returns.
const MAX_JOB_LOG_BYTES: usize = 64 * 1024;

/// Every job of the newest attempt of each workflow run GitHub recorded for
/// `commit`, with its conclusion and first failed step.
pub(super) fn ci_commit_jobs(input: &Value) -> Result<Value, OrbitError> {
    let commit = required_string(input, "commit")?;
    let workspace_path = Path::new(required_string(input, "workspace_path")?);
    if !valid_expected_remote_sha(Some(commit)) {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS commit jobs requires a full commit id, not '{commit}'"
        )));
    }
    let runs = gh_json(
        vec![
            "api".to_string(),
            "--method".to_string(),
            "GET".to_string(),
            "repos/{owner}/{repo}/actions/runs".to_string(),
            "-F".to_string(),
            format!("head_sha={commit}"),
            "-F".to_string(),
            "per_page=100".to_string(),
        ],
        workspace_path,
        "commit runs",
    )?;
    let mut jobs = Vec::new();
    for run in runs["workflow_runs"].as_array().into_iter().flatten() {
        let Some(run_id) = run["id"].as_u64() else {
            continue;
        };
        let listing = gh_json(
            vec![
                "api".to_string(),
                "--method".to_string(),
                "GET".to_string(),
                format!("repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs"),
                "-F".to_string(),
                "per_page=100".to_string(),
            ],
            workspace_path,
            "run jobs",
        )?;
        for job in listing["jobs"].as_array().into_iter().flatten() {
            let failed_step = job["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|step| step["conclusion"].as_str() == Some("failure"))
                .and_then(|step| step["name"].as_str());
            jobs.push(json!({
                "run_id": run_id,
                "job_id": job["id"],
                "workflow": run["name"],
                "name": job["name"],
                "status": job["status"],
                "conclusion": job["conclusion"],
                "failed_step": failed_step,
            }));
        }
    }
    Ok(json!({ "commit": commit, "jobs": jobs }))
}

/// One failed job's bounded, redacted failed-step log. `fallback_error` is
/// the redacted reason the read recovered no text, so the caller can tell an
/// unreadable log from a readable one.
pub(super) fn ci_job_log(input: &Value) -> Result<Value, OrbitError> {
    let run_id = required_string(input, "run_id")?;
    let job_id = required_string(input, "job_id")?;
    let workspace_path = required_string(input, "workspace_path")?;
    let requests = github_cli::RunLogRequests::from_input(
        &json!({"run": run_id, "job": job_id, "scope": "failed"}),
    )?
    .in_directory(workspace_path);
    let read = github_cli::read_run_log(
        &requests,
        github_cli::LogReadBounds::new(MAX_JOB_LOG_BYTES),
        None,
    )?;
    Ok(json!({
        "log": read.log.text,
        "truncated": read.log.truncated,
        "fallback_error": read.fallback_error,
    }))
}

/// Rerun one workflow run's failed jobs. Single-shot: the landing reruns a
/// failure at most once, and a retried request could queue a second rerun.
pub(super) fn ci_rerun_failed(input: &Value) -> Result<Value, OrbitError> {
    let run_id = required_string(input, "run_id")?;
    let workspace_path = required_string(input, "workspace_path")?;
    if !run_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(OrbitError::InvalidInput(format!(
            "invalid private automation VCS workflow run id '{run_id}'"
        )));
    }
    let result = execute(
        "gh",
        vec![
            "run".to_string(),
            "rerun".to_string(),
            run_id.to_string(),
            "--failed".to_string(),
        ],
        Some(Path::new(workspace_path)),
        DEFAULT_TIMEOUT_MS,
        "workflow rerun",
    )?;
    Ok(json!({ "stdout": result.stdout, "stderr": result.stderr }))
}

fn gh_json(args: Vec<String>, workspace_path: &Path, label: &str) -> Result<Value, OrbitError> {
    let result =
        execute_with_transient_retry("gh", &args, Some(workspace_path), DEFAULT_TIMEOUT_MS, label)?;
    serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS {label} returned invalid JSON: {error}"
        ))
    })
}
