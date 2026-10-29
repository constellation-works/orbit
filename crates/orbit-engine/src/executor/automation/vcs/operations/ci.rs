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

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::github_cli;
use serde_json::{Value, json};

use super::DEFAULT_TIMEOUT_MS;
use super::input::{required_string, valid_expected_remote_sha};
use super::process::execute_with_transient_retry;

/// Largest failed-job log excerpt one read returns.
const MAX_JOB_LOG_BYTES: usize = 64 * 1024;

// Project inside gh, before the process supervisor captures stdout. A page of
// repository metadata, commit messages or job-step timestamps can exceed its
// normal capture bound. Keep the original 100-row window and total_count so
// exact-tip completeness checks and bounded ancestry selection stay unchanged.
// Do not default a missing list to []: a malformed read must still fail.
const COMMIT_RUN_FIELDS: &str = "{total_count, workflow_runs: (.workflow_runs | if type != \"array\" then error(\"workflow_runs must be an array\") else map({id, name, workflow_id, repository: {id: .repository.id, full_name: .repository.full_name}, status, conclusion, run_started_at, run_attempt, event, head_branch, head_sha, pull_requests}) end)}";
const RUN_JOB_FIELDS: &str = "{total_count, jobs: (.jobs | if type != \"array\" then error(\"jobs must be an array\") else map({id, name, status, conclusion, started_at, completed_at, steps: (.steps | if type == \"array\" then map({name, number, status, conclusion}) else . end)}) end)}";

/// Every job of the newest attempt of each workflow run GitHub recorded for
/// `commit`, with its conclusion and first failed step. Base ancestry reads
/// retain completed successful pushes on the accepted base or its descendants;
/// the candidate rerun supplies the other half of attribution evidence.
pub(super) fn ci_commit_jobs(input: &Value) -> Result<Value, OrbitError> {
    let commit = required_string(input, "commit")?;
    let workspace_path = Path::new(required_string(input, "workspace_path")?);
    if !valid_expected_remote_sha(Some(commit)) {
        return Err(OrbitError::InvalidInput(format!(
            "private automation VCS commit jobs requires a full commit id, not '{commit}'"
        )));
    }
    // Exact-tip reads retain the latest push per workflow. Untouched-file
    // evidence reads retain the latest push per workflow and commit in the
    // accepted base's ancestry range, including superseded base tips.
    let push_branch = input.get("latest_push_branch").and_then(Value::as_str);
    let base_push_tip = input.get("base_push_tip").and_then(Value::as_str);
    if let Some(tip) = base_push_tip
        && (push_branch.is_none() || !valid_expected_remote_sha(Some(tip)))
    {
        return Err(OrbitError::InvalidInput(
            "base push history requires a branch and full tip commit id".into(),
        ));
    }
    let mut args = vec![
        "api".to_string(),
        "--method".to_string(),
        "GET".to_string(),
        "repos/{owner}/{repo}/actions/runs".to_string(),
        "-F".to_string(),
        "per_page=100".to_string(),
    ];
    if base_push_tip.is_none() {
        args.extend(["-F".into(), format!("head_sha={commit}")]);
    }
    if let Some(branch) = push_branch {
        args.extend([
            "-F".into(),
            "event=push".into(),
            "-F".into(),
            format!("branch={branch}"),
        ]);
    }
    let runs = projected_ci_json(args, workspace_path, "commit runs", COMMIT_RUN_FIELDS)?;
    let listed = runs["workflow_runs"]
        .as_array()
        .ok_or_else(|| OrbitError::Execution("commit runs carried no workflow run list".into()))?;
    let mut selected: Vec<&Value> = listed.iter().collect();
    if let Some(branch) = push_branch {
        if base_push_tip.is_none()
            && runs["total_count"]
                .as_u64()
                .is_none_or(|total| total > listed.len() as u64)
        {
            return Err(OrbitError::Execution(
                "base push runs were incomplete".into(),
            ));
        }
        let mut newest = BTreeMap::<(u64, &str), &Value>::new();
        for run in &selected {
            if run["event"] != "push" || run["head_branch"] != branch {
                continue;
            }
            let Some(sha) = run["head_sha"]
                .as_str()
                .filter(|sha| valid_expected_remote_sha(Some(sha)))
            else {
                continue;
            };
            if base_push_tip.is_none() && sha != commit {
                continue;
            }
            let (Some(workflow), Some(id)) = (run["workflow_id"].as_u64(), run["id"].as_u64())
            else {
                return Err(OrbitError::Execution(
                    "base push run omitted its identity".into(),
                ));
            };
            if newest
                .get(&(workflow, sha))
                .is_none_or(|prior| prior["id"].as_u64().is_some_and(|prior| id > prior))
            {
                newest.insert((workflow, sha), run);
            }
        }
        selected = newest.into_values().collect();
    }
    let mut jobs = Vec::new();
    for run in selected {
        // A partial or cancelled workflow cannot establish a passing base result.
        // The bounded recent-run window can miss evidence, which conservatively
        // keeps the base wait; it never treats a partial listing as a pass.
        if let Some(tip) = base_push_tip {
            if run["status"] != "completed" || run["conclusion"] != "success" {
                continue;
            }
            let sha = run["head_sha"].as_str().unwrap_or_default();
            if !super::super::git::git_command_success(
                workspace_path,
                &["merge-base", "--is-ancestor", commit, sha],
            )
            .unwrap_or(false)
                || !super::super::git::git_command_success(
                    workspace_path,
                    &["merge-base", "--is-ancestor", sha, tip],
                )
                .unwrap_or(false)
            {
                continue;
            }
        }
        let Some(run_id) = run["id"].as_u64() else {
            continue;
        };
        let listing = projected_ci_json(
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
            RUN_JOB_FIELDS,
        )?;
        let listed_jobs = listing["jobs"]
            .as_array()
            .ok_or_else(|| OrbitError::Execution("run jobs carried no job list".into()))?;
        if push_branch.is_some()
            && listing["total_count"]
                .as_u64()
                .is_none_or(|total| total > listed_jobs.len() as u64)
        {
            return Err(OrbitError::Execution(
                "base push jobs were incomplete".into(),
            ));
        }
        for job in listed_jobs {
            let failed_steps: Vec<Value> = job["steps"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|step| step["conclusion"].as_str() == Some("failure"))
                .map(|step| step["name"].clone())
                .collect();
            let failed_step = failed_steps.first().and_then(Value::as_str);
            let pull_requests = if run["pull_requests"].is_array() {
                run["pull_requests"].clone()
            } else {
                json!([])
            };
            let pull_request_numbers: Vec<Value> = pull_requests
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|pull_request| {
                    pull_request
                        .as_u64()
                        .or_else(|| pull_request["number"].as_u64())
                        .map(|number| json!(number))
                })
                .collect();
            jobs.push(json!({
                "run_id": run_id,
                "job_id": job["id"],
                "workflow": run["name"],
                "workflow_id":run["workflow_id"],
                "repository_id":run["repository"]["id"],
                "repository":run["repository"]["full_name"],
                "name": job["name"],
                "status": job["status"],
                "conclusion": job["conclusion"],
                "failed_step": failed_step,
                "failed_steps": failed_steps,
                "started_at": job["started_at"],
                "completed_at": job["completed_at"],
                "run_status": run["status"],
                "run_started_at": run["run_started_at"],
                "run_attempt": run["run_attempt"],
                "event": run["event"],
                "head_branch": run["head_branch"],
                "head_sha": run["head_sha"],
                "pull_requests": pull_requests,
                "pull_request_numbers": pull_request_numbers,
            }));
        }
    }
    Ok(json!({ "commit": commit, "jobs": jobs }))
}

/// One failed job's bounded, redacted failed-step log. `fallback_error` is
/// the redacted reason the read recovered no text, so the caller can tell an
/// unreadable log from a readable one. Complete command evidence or bounded
/// failure regions accompany the display with their source; callers must bind
/// them to the provider's failed job and step before recovery.
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
        "failure_regions": read.log.failure_regions,
        "source": read.source,
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
    if let Some(expected) = input.get("plan") {
        let plan: orbit_types::workflow::CiRerunPlan = serde_json::from_value(expected.clone())
            .map_err(|_| super::prerequisite::unreadable("CI retry reservation"))?;
        if plan.run_id.to_string() != run_id {
            return Err(super::prerequisite::unreadable("CI retry action run"));
        }
        let refreshed = super::ci_rerun::ci_rerun_plan(
            &json!({"workspace_path":workspace_path,"pr":plan.pr_number,"head_sha":plan.head_sha,"head_branch":plan.head_branch,"base_branch":plan.base_branch,"accepted_base":plan.accepted_base,"run_id":plan.run_id,"roots":plan.roots,"deadline_ms":input["deadline_ms"]}),
        )?;
        let refreshed: orbit_types::workflow::CiRerunPlan =
            serde_json::from_value(refreshed["plan"].clone())
                .map_err(|_| super::prerequisite::unreadable("refreshed CI retry plan"))?;
        if refreshed != plan {
            return Err(super::prerequisite::unreadable(
                "CI retry authorization changed before send",
            ));
        }
    }
    let deadline = input
        .get("prerequisite")
        .or_else(|| input.get("plan"))
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

fn projected_ci_json(
    mut args: Vec<String>,
    workspace_path: &Path,
    label: &str,
    fields: &str,
) -> Result<Value, OrbitError> {
    args.extend(["--jq".into(), fields.into()]);
    let result =
        execute_with_transient_retry("gh", &args, Some(workspace_path), DEFAULT_TIMEOUT_MS, label)?;
    serde_json::from_str(&result.stdout).map_err(|error| {
        OrbitError::Execution(format!(
            "private automation VCS {label} returned invalid JSON: {error}"
        ))
    })
}
