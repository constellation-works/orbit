use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskStatus};
use serde_json::{Value, json};

use crate::context::{ReviewReleaseRequest, RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::input_string_field;

use super::super::freshness::{rebase_belongs_to_attempt, rebase_provenance_summary};
use super::super::resume::ensure_retry_descends_from;
use super::conflict::{pipeline_checkpoint_string, pipeline_step};
use super::{FOREIGN_REBASE_EVENT, LANDING_REVIEW_STEPS, REVIEW_ADMISSION_STEPS};

pub(super) fn ensure_failure_handoff_ownership<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task: &orbit_types::task::Task,
    run_id: &str,
) -> Result<(), OrbitError> {
    let task_owner = task.job_run_id.as_deref().ok_or_else(|| {
        OrbitError::Execution(format!(
            "pr_failure_handoff: task '{}' has no owning run; refusing handoff for run '{}'",
            task.id, run_id
        ))
    })?;
    if task_owner == run_id {
        return Ok(());
    }

    let worktree = pipeline_step(input, "worktree")?;
    let checkpoint_owner = input_string_field(worktree, "job_run_id")
        .or_else(|| input_string_field(worktree, "batch_id"))
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "pr_failure_handoff: resumed run '{run_id}' has no worktree ownership checkpoint"
            ))
        })?;
    if task_owner != checkpoint_owner {
        return Err(OrbitError::Execution(format!(
            "pr_failure_handoff: task '{}' belongs to run '{}', not active run '{}' or worktree checkpoint owner '{}'",
            task.id, task_owner, run_id, checkpoint_owner
        )));
    }

    ensure_retry_descends_from(
        host,
        "pr_failure_handoff",
        "worktree checkpoint owner",
        &task.id,
        run_id,
        &checkpoint_owner,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn preserve_completion_failure<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_code: &str,
    error_message: &str,
    pr_number: &str,
) -> Result<Value, OrbitError> {
    let pr_url = task
        .external_refs
        .iter()
        .find(|external_ref| external_ref.system == "github-pr" && external_ref.id == pr_number)
        .and_then(|external_ref| external_ref.url.clone());
    // [ORB-14849] A before-landing review that did not approve settles here
    // too: its PR stays open and unmerged, under the step's typed reason.
    let landing_review = LANDING_REVIEW_STEPS
        .contains(&failed_step_id)
        .then(|| typed_reason(error_code, error_message));
    let note = match &landing_review {
        Some(reason) => format!(
            "Before-landing review did not approve PR #{pr_number} (`{reason}`); the PR stays \
             open and unmerged and the task stays '{}'. Nothing was merged, and only a fix the \
             review settled was pushed. An operator decides what lands next.\n\n- Run: \
             `{run_id}`\n- Failed step: `{failed_step_id}`\n- Error code: \
             `{error_code}`\n\nFailure:\n```text\n{error_message}\n```",
            task.status
        ),
        None => format!(
            "PR completion failed after publication; preserved PR #{pr_number} and task status \
             '{}' for a safe completion retry. No candidate, PR body, branch, or repository \
             setting was changed.\n\n- Run: `{run_id}`\n- Failed step: `{failed_step_id}`\n- \
             Error code: `{error_code}`\n\nFailure:\n```text\n{error_message}\n```",
            task.status
        ),
    };
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: note,
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": if landing_review.is_some() {
            "landing_review_failure"
        } else {
            "review_completion_failure"
        },
        "reason": landing_review,
        "failed_step_id": failed_step_id,
        "pr_number": pr_number,
        "pr_url": pr_url,
        "candidate_preserved": true,
        "task_status": task.status.to_string(),
    }))
}

/// The typed reason a failure message leads with — the first `snake_case:`
/// token, such as `review_gate_blocked` or `review_timeout_incomplete` —
/// else the step's error code.
fn typed_reason(error_code: &str, error_message: &str) -> String {
    error_message
        .split_whitespace()
        .filter_map(|word| word.strip_suffix(':'))
        .find(|word| {
            word.contains('_')
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .unwrap_or(error_code)
        .to_string()
}

/// Whether the in-progress rebase is the one `sync_base` started from this
/// run's `prepare_branch` checkpoint: its `orig-head`, `onto`, and
/// `head-name` must name the prepared head SHA, base SHA, and branch. Without
/// that checkpoint no Orbit step started a rebase, so none is owned.
pub(super) fn prepared_attempt_owns_rebase(
    input: &Value,
    workspace_path: &Path,
) -> Result<bool, OrbitError> {
    let (Some(head), Some(head_sha), Some(base_sha)) = (
        pipeline_checkpoint_string(input, "prepare_branch", "head"),
        pipeline_checkpoint_string(input, "prepare_branch", "head_sha"),
        pipeline_checkpoint_string(input, "prepare_branch", "base_sha"),
    ) else {
        return Ok(false);
    };
    rebase_belongs_to_attempt(workspace_path, &head, &head_sha, &base_sha)
}

/// Leave a rebase this run did not start untouched: no abort, commit, push,
/// or PR. The task is blocked with the rebase provenance so an operator can
/// inspect the worktree; the original step error stays authoritative.
pub(super) fn refuse_foreign_rebase<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_code: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let provenance = rebase_provenance_summary(workspace_path);
    let note = format!(
        "failure handoff refused a rebase this run did not start: run={run_id}, \
         failed_step={failed_step_id}, {provenance}; nothing was aborted, committed, pushed, \
         or published"
    );
    let message = format!(
        "{note}\n\nThe in-progress rebase does not match this run's prepared branch checkpoint, \
         so its rebase metadata, index, and worktree edits were left intact. Inspect \
         `{}` before retrying delivery.\n\n- Error code: `{error_code}`\n\n\
         Failure:\n```text\n{error_message}\n```",
        workspace_path.display()
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(FOREIGN_REBASE_EVENT.to_string()),
            status_note: Some(note),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message,
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": "foreign_rebase_refused",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "workspace_path": workspace_path,
        "rebase_provenance": provenance,
        "pr_created": false,
        "task_status": "blocked",
    }))
}

/// [ORB-13890] Close every review attempt this run admitted that has no
/// verdict yet, charging the reviewer runtime it spent, so a failed or
/// timed-out reviewer step never leaves its attempt open for the lineage.
/// Only the run's own admission checkpoints are read, so this needs no task
/// ownership proof. The original failure stays authoritative: a release
/// error is logged, not raised; the run's termination releases it again.
pub(super) fn release_review_attempts<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    run_id: &str,
) {
    for step in REVIEW_ADMISSION_STEPS {
        let Ok(admission) = pipeline_step(input, step) else {
            continue;
        };
        if admission.get("applies").and_then(Value::as_bool) != Some(true) {
            continue;
        }
        let (Some(lineage_key), Some(attempt_id)) = (
            input_string_field(admission, "lineage_key"),
            input_string_field(admission, "attempt_id"),
        ) else {
            continue;
        };
        let request = ReviewReleaseRequest {
            run_id: run_id.to_string(),
            lineage_key,
            attempt_id,
        };
        if let Err(error) = host.release_review_attempt(&request) {
            tracing::warn!(
                run_id = %run_id,
                attempt_id = %request.attempt_id,
                error = %error,
                "pr_failure_handoff could not release the review attempt; run termination releases it"
            );
        }
    }
}
