use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskStatus};
use orbit_types::workflow::ReviewGateAdmissionOutput;
use serde_json::{Value, json};

use crate::context::{ReviewReleaseRequest, RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string,
};

use super::super::freshness::{commit_sha, rebase_belongs_to_attempt, rebase_provenance_summary};
use super::super::push::push_batch_changes_inner;
use super::super::resume::ensure_retry_descends_from;
use super::conflict::{pipeline_checkpoint_string, pipeline_step};
use super::preserve::recorded_spec_digest;
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

/// The settlement step of the before-landing review.
const LANDING_SETTLE_STEP: &str = "landing_review_gate_settle";

/// The refusal a before-landing settlement leads with when the verdict is
/// `incomplete`.
const INCOMPLETE_VERDICT_REFUSAL: &str = "review_gate_blocked: verdict incomplete";

/// [ORB-15329] Push the reviewer commit a before-landing review recorded
/// before it settled `incomplete`, so it is not stranded in this host's
/// worktree.
///
/// Settlement commits the reviewer's repair before it judges the verdict, so
/// an `incomplete` refusal leaves that commit on the worktree branch and
/// `landing_push` never runs. It goes onto the published head the review
/// reviewed under a lease on it and nowhere else. This is a preservation
/// push: the commit is neither approved nor revalidated, the PR stays open
/// and unmerged, and `complete_pr` does not run. `None` when the failure is
/// not an incomplete settlement; otherwise the outcome, where a push that
/// could not be made is recorded rather than raised so the original failure
/// stays authoritative.
pub(super) fn push_incomplete_landing_reviewer_commit<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    failed_step_id: &str,
    error_message: &str,
) -> Option<Value> {
    if failed_step_id != LANDING_SETTLE_STEP || !error_message.contains(INCOMPLETE_VERDICT_REFUSAL)
    {
        return None;
    }
    Some(
        push_landing_reviewer_commit(host, input)
            .unwrap_or_else(|error| json!({"decision": "push_failed", "error": error.to_string()})),
    )
}

fn push_landing_reviewer_commit<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let worktree = pipeline_step(input, "worktree")?;
    let workspace_path = canonicalize_existing_dir(
        required_input_string(worktree, "workspace_path")?,
        "pipeline.worktree.workspace_path",
    )?;
    let missing = |step: &str, field: &str| {
        OrbitError::Execution(format!(
            "pr_failure_handoff: the run recorded no `{step}.{field}`"
        ))
    };
    let branch = pipeline_checkpoint_string(input, "sync_base", "head")
        .ok_or_else(|| missing("sync_base", "head"))?;
    let published = pipeline_checkpoint_string(input, "push", "local_sha")
        .ok_or_else(|| missing("push", "local_sha"))?;
    if commit_sha(&workspace_path, &branch)? == published {
        return Ok(json!({"decision": "no_reviewer_commit", "published_head_sha": published}));
    }
    let mut pushed = push_batch_changes_inner(
        host,
        &json!({
            "branch": branch,
            "workspace_path": workspace_path,
            "lease_remote_sha": published,
        }),
        &workspace_path,
    )?;
    pushed["published_head_sha"] = json!(published);
    Ok(pushed)
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
    reviewer_push: Option<&Value>,
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
             open and unmerged and the task stays '{}'. Nothing was merged. {} An operator \
             decides what lands next.\n\n- Run: `{run_id}`\n- Failed step: `{failed_step_id}`\n- Error code: \
             `{error_code}`\n\nFailure:\n```text\n{error_message}\n```",
            task.status,
            reviewer_push_sentence(reviewer_push),
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
                operation_id: None,
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
        "reviewer_push": reviewer_push,
        // An operator requeue of the task this run's final recovery blocked
        // resumes the kept PR only for the run and specification named here.
        "task_id": task.id,
        "handoff_run_id": run_id,
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
        "failed_step_id": failed_step_id,
        "pr_number": pr_number,
        "pr_url": pr_url,
        "candidate_preserved": true,
        "task_status": task.status.to_string(),
    }))
}

/// What the comment says became of the reviewer commit an incomplete
/// before-landing review recorded.
fn reviewer_push_sentence(reviewer_push: Option<&Value>) -> String {
    let Some(push) = reviewer_push else {
        return "Only a fix the review settled was pushed.".to_string();
    };
    let decision = push
        .get("decision")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let field = |key: &str| push.get(key).and_then(Value::as_str).unwrap_or("unknown");
    match decision {
        "no_reviewer_commit" => "The review recorded no commit to push.".to_string(),
        "push_failed" => format!(
            "The reviewer's commit could not be pushed and stays in the run's worktree: {}",
            field("error")
        ),
        _ => format!(
            "The reviewer's commit `{}` was pushed to the PR branch on top of the published head \
             `{}` so it is not stranded in the run's worktree; it is neither approved nor \
             revalidated, and hosted CI restarts on it.",
            field("local_sha"),
            field("published_head_sha"),
        ),
    }
}

/// The typed reason for a failed step: a review-gate refusal's code, else the
/// first `snake_case:` token its message leads with, such as
/// `push_lease_lost`, else the step's error code.
fn typed_reason(error_code: &str, error_message: &str) -> String {
    if let Some(refusal) = orbit_types::workflow::ReviewGateRefusal::from_error_code(error_code) {
        return refusal.error_code().to_string();
    }
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
                operation_id: None,
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
        let Some(admission) = pipeline_step(input, step)
            .ok()
            .and_then(|output| ReviewGateAdmissionOutput::from_value(output).ok())
        else {
            continue;
        };
        let Some(admitted) = admission.admitted() else {
            continue;
        };
        let (lineage_key, attempt_id) = (admitted.lineage_key.trim(), admitted.attempt_id.trim());
        if lineage_key.is_empty() || attempt_id.is_empty() {
            continue;
        }
        let request = ReviewReleaseRequest {
            run_id: run_id.to_string(),
            lineage_key: lineage_key.to_string(),
            attempt_id: attempt_id.to_string(),
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
