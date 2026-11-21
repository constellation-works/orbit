use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workflow::PrInfrastructureEvidence;
use serde_json::{Value, json};

use super::super::super::completion_deadline::Deadline;
use super::super::input::{number, required_string, text};
use super::snapshot::{PullRequestSelector, checked_run, poll_interval_ms, snapshot};
use super::support::{complete_list, unreadable};
use super::workflow::gate_passed;

/// Observe the reserved attempt and all current gates before any completion.
/// A missing gate is never inferred to pass from the PR's CLEAN state.
/// Observation only reads: a rate-limited read waits for the reset within the
/// deadline and observes again, and never resends the reserved mutation.
pub(in super::super) fn observe(input: &Value) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let deadline = Deadline::from_input(input)?;
    super::super::read_reset::read_through_throttles(deadline, workspace, |pass| {
        observe_once(input, pass)
    })
}

fn observe_once(
    input: &Value,
    pass: &super::super::read_reset::SnapshotWaits,
) -> Result<Value, OrbitError> {
    let workspace = Path::new(required_string(input, "workspace_path")?);
    let evidence: PrInfrastructureEvidence = serde_json::from_value(input["evidence"].clone())
        .map_err(|_| unreadable("reserved evidence"))?;
    let deadline = Deadline::from_input(input)?;
    if let Some(failure) = evidence.failed_workflow_run() {
        if failure.candidate_owned {
            return Err(unreadable(
                "candidate-owned workflow failure cannot authorize infrastructure recovery",
            ));
        }
        return super::super::failed_workflow::observe_failed_workflow(
            deadline,
            workspace,
            &evidence,
            poll_interval_ms(input),
            pass,
        );
    }
    if evidence.required.is_empty() {
        return Err(unreadable("reserved protected contexts"));
    }
    let snapshot = snapshot(
        deadline,
        workspace,
        PullRequestSelector {
            pr: &evidence.pr_number,
            head: &evidence.head_sha,
            branch: &evidence.head_branch,
            base: &evidence.base_branch,
        },
        poll_interval_ms(input),
        pass,
    )?;
    if snapshot.repository != evidence.repository
        || snapshot.repository_id != evidence.repository_id
        || snapshot.required != evidence.required
        || snapshot.pr["base"]["sha"] != evidence.base_sha
    {
        return Err(unreadable(
            "policy/repository/base changed since reservation",
        ));
    }
    let runs = complete_list(&snapshot.runs, "workflow_runs")?;
    if runs.iter().any(|run| {
        run["workflow_id"].as_u64() == Some(evidence.workflow_id)
            && run["id"].as_u64().is_none_or(|id| id > evidence.run_id)
    }) {
        return Err(unreadable("reserved workflow run was superseded"));
    }
    let listed = runs
        .iter()
        .find(|run| run["id"].as_u64() == Some(evidence.run_id))
        .ok_or_else(|| unreadable("reserved run disappeared"))?;
    let run = checked_run(deadline, workspace, &snapshot, listed)?;
    if number(&run, "workflow_id", unreadable)? != evidence.workflow_id
        || text(&run, "path", unreadable)? != evidence.workflow_path
        || text(&run, "event", unreadable)? != evidence.event
        || run["actor"] != evidence.run["actor"]
    {
        return Err(unreadable("reserved workflow/event changed"));
    }
    let attempt = number(&run, "run_attempt", unreadable)?;
    if attempt < u64::from(evidence.run_attempt) || attempt > u64::from(evidence.run_attempt) + 1 {
        return Err(unreadable("superseded/ambiguous reserved attempt"));
    }
    let checks = complete_list(&snapshot.checks, "check_runs")?;
    let statuses = complete_list(&snapshot.statuses, "statuses")?;
    let passed = snapshot
        .required
        .iter()
        .all(|gate| gate_passed(gate, checks, statuses))
        && checks.iter().any(|check| {
            check["name"].as_str() == Some(&evidence.missing.context)
                && check["app"]["id"].as_u64() == Some(evidence.app_id)
                && check["check_suite"]["id"] == run["check_suite_id"]
                && check["status"] == "completed"
                && check["conclusion"] == "success"
        });
    let state = text(&run, "status", unreadable)?;
    if !matches!(
        state,
        "completed" | "queued" | "in_progress" | "waiting" | "requested" | "pending"
    ) {
        return Err(unreadable("unknown attempt state"));
    }
    if attempt == u64::from(evidence.run_attempt)
        && (state != "completed" || run["conclusion"] != "failure")
    {
        return Err(unreadable("original attempt changed without a rerun"));
    }
    if attempt > u64::from(evidence.run_attempt)
        && state == "completed"
        && (run["conclusion"] != "success" || !passed)
    {
        return Err(unreadable(
            "second failure or protected check still missing",
        ));
    }
    deadline.remaining()?;
    Ok(
        json!({"attempt":attempt,"status":state,"conclusion":run["conclusion"],"gates_passed": passed && attempt > u64::from(evidence.run_attempt) && state == "completed", "run":run,"checks":snapshot.checks,"statuses":snapshot.statuses}),
    )
}
