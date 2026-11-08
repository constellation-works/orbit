//! [ORB-23003] Landing follow-through for an owner-local delivery left in
//! review: watch its pull request's checks and record what they show.
//!
//! A follower's handoff is landed by the owner's landing job, which waits on
//! the pull request's checks and records each classified failure on the
//! task. An owner-local delivery that ends in review (a dashboard ship,
//! `orbit run ship`, a drain leaf without `--complete` or `--auto-fix`) has no
//! handoff, so nothing used to watch its pull request: a red one sat in
//! review with no sign on the task. This activity is that watch.
//!
//! It polls the pull request until its checks settle or its budget runs out,
//! classifies failed checks exactly as the landing does
//! ([`classify_watched_failures`]), and appends one
//! [`LANDING_CHECKS_OBSERVED_EVENT`] to the task's history. Logged transient
//! failures use completion's durable, bounded rerun path before observation.
//! It never merges, updates the branch, starts a repair or changes status.
//! A task that left review, or a pull request that merged or closed, ends
//! the watch without an entry.

use std::path::{Path, PathBuf};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::task::{TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::handoff::LANDING_CHECKS_OBSERVED_EVENT;
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::super::ci::bounded_u64;
use super::super::super::input::input_string_field;
use super::super::completion_deadline::now_ms;
use super::super::freshness::commit_sha;
use super::super::git::{base_sync_mode_from_input, git_output, resolve_worktree_start_point};
use super::super::pr::{
    CheckState, PrMergeState, PullRequestStatus, classify_pr_state, read_pr_status_before,
};
use super::super::remote_probe::fixed_wait;
use super::checks::{classify_watched_failures, failed_checks};

const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const MAX_WAIT_SECONDS: u64 = 6 * 60 * 60;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
const MAX_POLL_INTERVAL_SECONDS: u64 = 10 * 60;

/// Who the history entry names.
const WATCH_ACTOR: &str = "owner-landing";

pub(in crate::executor::automation) fn pr_checks_watch<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let task_id = watched_task_id(input)
        .ok_or_else(|| invalid("task_id, or task_ids naming one task, is required"))?;
    let pr_number = input
        .get("pr_number")
        .and_then(|value| match value {
            Value::String(text) => Some(text.trim().to_string()),
            Value::Number(number) => number.as_u64().map(|n| n.to_string()),
            _ => None,
        })
        .filter(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| invalid("pr_number must name a pull request"))?;
    let workspace = match input_string_field(input, "workspace_path") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(host.repo_root()?),
    };
    let max_wait_seconds = bounded_u64(
        input,
        "max_wait_seconds",
        DEFAULT_MAX_WAIT_SECONDS,
        MAX_WAIT_SECONDS,
    )?;
    let poll_interval_seconds = bounded_u64(
        input,
        "poll_interval_seconds",
        DEFAULT_POLL_INTERVAL_SECONDS,
        MAX_POLL_INTERVAL_SECONDS,
    )?
    .max(MIN_POLL_INTERVAL_SECONDS);
    let deadline_ms = now_ms()?.saturating_add(max_wait_seconds.saturating_mul(1000).max(60_000));

    loop {
        if !in_review(host, &task_id)? {
            return Ok(ended(&task_id, &pr_number, "task_left_review"));
        }
        let status = read_pr_status_before(host, &workspace, &pr_number, deadline_ms)?;
        let expired = max_wait_seconds == 0 || now_ms()? >= deadline_ms;
        let observed = match classify_pr_state(&status) {
            PrMergeState::Merged => return Ok(ended(&task_id, &pr_number, "merged")),
            PrMergeState::Closed => return Ok(ended(&task_id, &pr_number, "closed")),
            PrMergeState::Contradictory(why) => {
                observation(&status, &pr_number, "blocked", Some(why))
            }
            PrMergeState::Conflict => observation(
                &status,
                &pr_number,
                "conflicting",
                Some("the pull request conflicts with its base".to_string()),
            ),
            PrMergeState::Mergeable => observation(&status, &pr_number, "passed", None),
            PrMergeState::Pending if !expired => {
                fixed_wait(Duration::from_secs(poll_interval_seconds));
                continue;
            }
            PrMergeState::Pending => observation(
                &status,
                &pr_number,
                "pending",
                Some(format!(
                    "the checks had not settled after {max_wait_seconds}s"
                )),
            ),
            PrMergeState::Blocked(block) => {
                let failed = failed_checks(&status);
                let running = checks_running(&status);
                if running && !expired {
                    fixed_wait(Duration::from_secs(poll_interval_seconds));
                    continue;
                }
                if failed.is_empty() {
                    observation(&status, &pr_number, "blocked", Some(block.to_string()))
                } else {
                    let mut retry_input = input.clone();
                    retry_input["workspace_path"] = json!(workspace);
                    let hold = if expired {
                        None
                    } else {
                        match super::super::pr::recover_review_ci(
                            host,
                            &retry_input,
                            &task_id,
                            &pr_number,
                            &status,
                            deadline_ms,
                        )? {
                            super::super::pr::ReviewCiRetryStep::PollAgain => {
                                fixed_wait(Duration::from_secs(poll_interval_seconds));
                                continue;
                            }
                            super::super::pr::ReviewCiRetryStep::Hold(reason) => Some(reason),
                            super::super::pr::ReviewCiRetryStep::Observe => None,
                        }
                    };
                    let mut observed = failed_observation(
                        host,
                        input,
                        &workspace,
                        &status,
                        &pr_number,
                        &failed,
                        deadline_ms,
                    );
                    if let Some(reason) = hold {
                        observed["hold_reason"] = json!(reason);
                    }
                    observed
                }
            }
        };
        record(host, &task_id, &observed)?;
        return Ok(json!({
            "task_id": task_id,
            "pr_number": pr_number,
            "recorded": true,
            "observation": observed,
        }));
    }
}

/// The one task watched: `task_id`, or a `task_ids` list of exactly one, as
/// the delivery leaf forwards its own input.
fn watched_task_id(input: &Value) -> Option<String> {
    input_string_field(input, "task_id")
        .filter(|id| !id.trim().is_empty())
        .or_else(|| match input.get("task_ids")?.as_array()?.as_slice() {
            [only] => only.as_str().map(str::to_string),
            _ => None,
        })
}

fn invalid(message: &str) -> OrbitError {
    OrbitError::InvalidInput(format!("pr_checks_watch: {message}"))
}

fn ended(task_id: &str, pr_number: &str, reason: &str) -> Value {
    json!({"task_id": task_id, "pr_number": pr_number, "recorded": false, "ended": reason})
}

fn in_review<H: RuntimeHost + ?Sized>(host: &H, task_id: &str) -> Result<bool, OrbitError> {
    Ok(host.get_task(task_id)?.status == TaskStatus::Review)
}

/// Whether any check in the rollup has not concluded.
fn checks_running(status: &PullRequestStatus) -> bool {
    status
        .status_checks
        .iter()
        .flatten()
        .any(|check| check.state == Some(CheckState::Pending))
}

fn observation(
    status: &PullRequestStatus,
    pr_number: &str,
    state: &str,
    reason: Option<String>,
) -> Value {
    let mut observed = json!({
        "state": state,
        "pr_number": pr_number,
        "head_sha": status.head_sha,
    });
    if let Some(reason) = reason {
        observed["reason"] = json!(reason);
    }
    observed
}

/// The `failed` observation: the failed checks by name, and the landing's
/// classification of them against the pull request's fork point and the
/// base branch's current tip.
fn failed_observation<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace: &Path,
    status: &PullRequestStatus,
    pr_number: &str,
    failed: &[(Option<String>, String)],
    deadline_ms: u64,
) -> Value {
    let mut observed = observation(status, pr_number, "failed", None);
    observed["failed_checks"] = json!(
        failed
            .iter()
            .map(|(workflow, name)| match workflow {
                Some(workflow) => format!("{workflow} / {name}"),
                None => name.clone(),
            })
            .collect::<Vec<_>>()
    );
    let classified = match (
        status.head_sha.as_deref(),
        fork_point(input, workspace, status),
    ) {
        (None, _) => (
            "inconclusive",
            "the failure was not classified (the pull request reported no head)".to_string(),
        ),
        (Some(_), Err(why)) => (
            "inconclusive",
            format!("the failure was not classified ({why})"),
        ),
        (Some(head), Ok((base, base_branch, tip))) => {
            let watched = classify_watched_failures(
                host,
                workspace,
                head,
                &base,
                &base_branch,
                Some(&tip),
                failed,
                Some(deadline_ms),
            );
            (watched.class, watched.detail)
        }
    };
    observed["classification"] = json!(classified.0);
    observed["detail"] = json!(classified.1);
    observed
}

/// The pull request's fork point from its base branch, that branch's name,
/// and the branch's current tip in the owner checkout.
fn fork_point(
    input: &Value,
    workspace: &Path,
    status: &PullRequestStatus,
) -> Result<(String, String, String), String> {
    let base_name = status
        .base_branch
        .clone()
        .or_else(|| input_string_field(input, "base"))
        .ok_or_else(|| "neither the pull request nor the input names its base".to_string())?;
    let head = status
        .head_sha
        .as_deref()
        .ok_or_else(|| "the pull request reported no head".to_string())?;
    let sync = base_sync_mode_from_input(input).map_err(|error| error.to_string())?;
    let base_ref = resolve_worktree_start_point(workspace, &base_name, sync)
        .map_err(|error| format!("base '{base_name}' could not be resolved: {error}"))?;
    let tip = commit_sha(workspace, &base_ref).map_err(|error| error.to_string())?;
    let base = git_output(workspace, &["merge-base", head, &tip])
        .map_err(|error| format!("the fork point of {head} could not be read: {error}"))?;
    let branch = base_name
        .strip_prefix("origin/")
        .unwrap_or(&base_name)
        .to_string();
    Ok((base.trim().to_string(), branch, tip))
}

/// Append the observation once: an identical note already recorded (a
/// resumed or repeated watch of the same head) is not written again.
fn record<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
    observed: &Value,
) -> Result<(), OrbitError> {
    let note = serde_json::to_string(observed)
        .map_err(|error| OrbitError::Execution(format!("pr_checks_watch: {error}")))?;
    if host.get_task_history(task_id)?.iter().any(|entry| {
        entry.event == LANDING_CHECKS_OBSERVED_EVENT && entry.note.as_deref() == Some(&note)
    }) {
        return Ok(());
    }
    host.apply_task_automation_update(
        task_id,
        TaskAutomationUpdate {
            append_history: vec![TaskHistoryEntry {
                at: chrono::Utc::now(),
                by: WATCH_ACTOR.into(),
                event: LANDING_CHECKS_OBSERVED_EVENT.into(),
                note: Some(note),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..TaskAutomationUpdate::default()
        },
    )
}
