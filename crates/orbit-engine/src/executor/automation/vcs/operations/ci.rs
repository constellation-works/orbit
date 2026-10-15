//! [ORB-15956] The hosted-CI reads and the one rerun the owner's landing makes
//! when a pull request's check fails: which jobs ran on a commit, a failed
//! job's bounded log, and a rerun of a run's failed jobs.
//!
//! Missing-required prerequisite recovery uses the same rerun operation with
//! typed evidence and a durable owner reservation; this adapter rechecks its
//! exact identities, source, latest attempt and deadline before mutation.
//!
//! The log read is the CI sweep's own (`orbit_tools::github_cli`), so landing
//! classifies a failure from exactly the excerpt the sweep would file.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::github_cli;
use serde_json::{Value, json};

use super::DEFAULT_TIMEOUT_MS;
use super::input::{required_string, valid_expected_remote_sha};
use super::process::execute_with_transient_retry;

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
    let deadline = input
        .get("deadline_ms")
        .map(|_| super::super::completion_deadline::Deadline::from_input(input))
        .transpose()?;
    let read = github_cli::read_run_log_guarded(
        &requests,
        github_cli::LogReadBounds::new(MAX_JOB_LOG_BYTES),
        None,
        &|request| {
            let mut request = request.clone();
            if let Some(deadline) = deadline {
                request.timeout_ms = Some(deadline.remaining()?);
            }
            Ok(request)
        },
    )?;
    if let Some(deadline) = deadline {
        super::super::completion_deadline::probe("prerequisite log", deadline.remaining()?);
        deadline.remaining()?;
    }
    Ok(json!({
        "log": read.log.text,
        "truncated": read.log.truncated,
        "source_complete": read.log.source_complete,
        "diagnostic": read.log.diagnostic,
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
    let deadline = input
        .get("prerequisite")
        .map(|_| super::super::completion_deadline::Deadline::from_input(input))
        .transpose()?;
    if let Some(deadline) = deadline {
        deadline.remaining()?;
    }
    if let Some(evidence) = input.get("prerequisite") {
        let expected: orbit_types::workflow::PrInfrastructureEvidence =
            serde_json::from_value(evidence.clone())
                .map_err(|_| super::prerequisite::unreadable("rerun reservation"))?;
        if expected.run_id.to_string() != run_id {
            return Err(super::prerequisite::unreadable("rerun run identity"));
        }
        let observed = super::prerequisite::observe(
            &json!({"workspace_path":workspace_path,"evidence":expected,"deadline_ms":input["deadline_ms"]}),
        )?;
        if observed["attempt"].as_u64() != Some(u64::from(expected.run_attempt))
            || observed["status"] != "completed"
            || observed["conclusion"] != "failure"
        {
            return Err(super::prerequisite::unreadable(
                "attempt advanced before mutation",
            ));
        }
        let refreshed = super::prerequisite::evidence(
            &json!({"workspace_path":workspace_path,"pr":expected.pr_number,"head_sha":expected.head_sha,"head_branch":expected.head_branch,"base_branch":expected.base_branch,"deadline_ms":input["deadline_ms"]}),
        )?;
        let refreshed: orbit_types::workflow::PrInfrastructureEvidence =
            serde_json::from_value(refreshed["evidence"].clone())
                .map_err(|_| super::prerequisite::unreadable("refreshed prerequisite"))?;
        if refreshed != expected {
            return Err(super::prerequisite::unreadable(
                "evidence changed before mutation",
            ));
        }
    }
    let timeout_ms = match deadline {
        Some(deadline) => deadline.remaining()?,
        None => DEFAULT_TIMEOUT_MS,
    };
    let result = super::process::run_vcs_process(
        "gh",
        vec![
            "run".to_string(),
            "rerun".to_string(),
            run_id.to_string(),
            "--failed".to_string(),
        ],
        Some(Path::new(workspace_path)),
        timeout_ms,
    )?;
    if input.get("prerequisite").is_some()
        && !result.success
        && !result.timed_out
        && ["(HTTP 401)", "(HTTP 403)"]
            .iter()
            .any(|status| result.stderr.trim_end().ends_with(status))
    {
        return Ok(json!({"refusal":"credential_or_permission","diagnostic":result.stderr}));
    }
    let result = super::process::succeeded(result, "workflow rerun")?;
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
