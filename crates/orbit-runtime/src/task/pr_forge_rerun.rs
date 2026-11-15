//! Bounded workflow observation before re-requesting failed jobs.

use std::path::Path;
use std::time::Duration;
#[cfg(not(test))]
use std::time::Instant;

use orbit_common::OrbitError;
use serde_json::Value;

#[cfg(not(test))]
use super::pr_forge as gh;
use super::pr_forge::CheckRerunOutcome;
#[cfg(test)]
use super::tests::pr_forge_rerun as gh;
#[cfg(test)]
use super::tests::pr_forge_rerun::{workflow_rerun_now, workflow_rerun_sleep};
#[cfg(not(test))]
use std::thread::sleep as workflow_rerun_sleep;

// A failed job can be visible before its workflow finishes. Allow the other
// jobs to settle, but do not hold synchronous recovery indefinitely.
const WORKFLOW_WAIT_LIMIT: Duration = Duration::from_secs(120);
const WORKFLOW_POLL_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(not(test))]
fn workflow_rerun_now() -> Instant {
    Instant::now()
}

pub(super) fn rerun_failed_head_workflows(
    repo_root: &Path,
    number: u64,
    head_sha: &str,
) -> Result<CheckRerunOutcome, OrbitError> {
    let started = workflow_rerun_now();
    let mut in_progress = Vec::new();
    // Observe all running workflows before sending any requests, so waiting
    // cannot cause a workflow we just reran to be requested a second time.
    let failed = loop {
        let remaining = WORKFLOW_WAIT_LIMIT.saturating_sub(workflow_rerun_now() - started);
        if remaining.is_zero() {
            return Err(no_rerunnable_workflow(number, head_sha, &in_progress));
        }
        let mut request = orbit_tools::github_cli::commit_workflow_runs_request(head_sha);
        let remaining_ms = remaining.as_millis().max(1) as u64;
        request.timeout_ms = Some(request.timeout_ms.unwrap_or(remaining_ms).min(remaining_ms));
        let stdout = match gh::task_forge_gh_request(request, repo_root, "gh run list") {
            Ok(stdout) => stdout,
            Err(_)
                if !in_progress.is_empty()
                    && workflow_rerun_now() - started >= WORKFLOW_WAIT_LIMIT =>
            {
                return Err(no_rerunnable_workflow(number, head_sha, &in_progress));
            }
            Err(error) => return Err(error),
        };
        let runs = orbit_tools::github_cli::parse_gh_json(&stdout, "gh run list")?;
        let runs = runs.as_array().ok_or_else(|| {
            OrbitError::Execution("gh run list did not return an array".to_string())
        })?;
        let on_head = runs
            .iter()
            .filter(|run| run.get("headSha").and_then(Value::as_str) == Some(head_sha))
            .collect::<Vec<_>>();
        in_progress = on_head
            .iter()
            .filter(|run| run.get("status").and_then(Value::as_str) == Some("in_progress"))
            .map(|run| {
                format!(
                    "{} (run {})",
                    run.get("workflowName")
                        .and_then(Value::as_str)
                        .unwrap_or("unnamed workflow"),
                    run.get("databaseId").unwrap_or(&Value::Null),
                )
            })
            .collect();
        let failed = on_head
            .iter()
            .filter(|run| {
                run.get("status").and_then(Value::as_str) == Some("completed")
                    && matches!(
                        run.get("conclusion").and_then(Value::as_str),
                        Some("failure" | "cancelled" | "timed_out" | "startup_failure")
                    )
            })
            .filter_map(|run| {
                Some((
                    run.get("databaseId")?.as_u64()?,
                    run.get("workflowName")
                        .and_then(Value::as_str)
                        .unwrap_or("unnamed workflow")
                        .to_string(),
                ))
            })
            .collect::<Vec<_>>();
        if in_progress.is_empty() {
            if failed.is_empty() {
                return Err(no_rerunnable_workflow(number, head_sha, &[]));
            }
            break failed;
        }
        let remaining = WORKFLOW_WAIT_LIMIT.saturating_sub(workflow_rerun_now() - started);
        if remaining.is_zero() {
            return Err(no_rerunnable_workflow(number, head_sha, &in_progress));
        }
        workflow_rerun_sleep(WORKFLOW_POLL_INTERVAL.min(remaining));
    };
    let mut rerun = Vec::new();
    for (run_id, name) in failed {
        if let Err(error) = gh::task_forge_gh_request(
            orbit_tools::github_cli::rerun_failed_jobs_request(run_id),
            repo_root,
            "gh run rerun",
        ) {
            return Ok(CheckRerunOutcome {
                requested: rerun,
                uncertain: Some((run_id, name, error.to_string())),
            });
        }
        rerun.push((run_id, name));
    }
    Ok(CheckRerunOutcome {
        requested: rerun,
        uncertain: None,
    })
}

fn no_rerunnable_workflow(number: u64, head_sha: &str, in_progress: &[String]) -> OrbitError {
    let mut message =
        format!("no failed workflow run on head {head_sha} of pull request #{number} can be rerun");
    if !in_progress.is_empty() {
        message.push_str(&format!(
            "; timed out after {}s waiting for in_progress workflows: {}",
            WORKFLOW_WAIT_LIMIT.as_secs(),
            in_progress.join(", ")
        ));
    }
    OrbitError::Execution(message)
}
