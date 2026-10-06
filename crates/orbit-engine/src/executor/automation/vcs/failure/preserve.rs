use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskStatus};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::super::commit::commit_failure_candidate;
use super::super::git::git_output;
use super::{MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES, VALIDATION_ENVIRONMENT_EVENT};

/// The spec digest a later run compares before resuming this candidate
/// [ORB-13985], read after this handoff's own selector widening.
pub(super) fn recorded_spec_digest<H: RuntimeHost + ?Sized>(
    host: &H,
    task_id: &str,
) -> Result<String, OrbitError> {
    Ok(host.get_task(task_id)?.spec_digest())
}

/// Keep a worktree whose implementer declared a blocker [ORB-14269].
///
/// Unlike a validation-environment failure, the tree may still be dirty:
/// nothing is committed, pushed, or published. The task is blocked under
/// [`TASK_BLOCKED_BY_AGENT_EVENT`](orbit_types::workflow::TASK_BLOCKED_BY_AGENT_EVENT)
/// with the kind in the note. Resume does not undo that block.
pub(super) fn preserve_agent_blocked_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let kind = orbit_types::workflow::task_blocked_by_agent_kind(error_message)
        .unwrap_or("unspecified")
        .to_string();
    let diagnostic = error_message.trim();
    let cut = orbit_common::text::floor_char_boundary(
        diagnostic,
        MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES,
    );
    let note = format!(
        "{} kind={kind} implementer declared a blocker: run={run_id}, \
         failed_step={failed_step_id}, candidate={head_sha}, branch={branch}; no PR was opened",
        orbit_types::workflow::TASK_BLOCKED_BY_AGENT_MARKER
    );
    let body = format!(
        "## Implementer blocker\n\nThe implementer stopped with kind `{kind}` and asked the run \
         not to continue. No repair ran, no review budget was spent, nothing was committed or \
         pushed, and no PR was opened. The worktree still holds the candidate as the implementer \
         left it, including uncommitted files.\n\n- Run: `{run_id}`\n- Failed step: \
         `{failed_step_id}`\n- Kind: `{kind}`\n- Branch: `{branch}`\n- Head: `{head_sha}`\n\n\
         Move the task back to `in-progress` once the blocker is gone, then resume the run if the \
         candidate should continue.\n\n## Failure\n\n```text\n{}{}\n```",
        &diagnostic[..cut],
        if cut < diagnostic.len() {
            format!("\n[truncated to {cut} of {} bytes]", diagnostic.len())
        } else {
            String::new()
        }
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(orbit_types::workflow::TASK_BLOCKED_BY_AGENT_EVENT.to_string()),
            status_note: Some(note.clone()),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: format!("{note}\n\n{body}"),
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": "blocked_by_agent",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "blocker_kind": kind,
        "candidate_preserved": true,
        "pr_created": false,
        "task_status": "blocked",
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    }))
}

/// Keep a candidate whose required validation could not run because a tool
/// was missing [ORB-13987].
///
/// The validation step only runs on a clean, committed candidate, so there is
/// nothing to commit and nothing to push: the branch and the run's worktree
/// already hold it, and `orbit job resume` reruns validation on that exact
/// head once the environment is fixed. The task is blocked under its own
/// event, which the blocked-task recovery backstop does not treat as a code
/// failure, and no PR is opened.
pub(super) fn preserve_validation_environment_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let diagnostic = error_message.trim();
    let cut = orbit_common::text::floor_char_boundary(
        diagnostic,
        MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES,
    );
    let note = format!(
        "required validation lacked a tool in its environment: run={run_id}, \
         failed_step={failed_step_id}, candidate={head_sha}, branch={branch}; the candidate was \
         not judged and no PR was opened"
    );
    let body = format!(
        "## Validation environment\n\nA required validation command could not run because a \
         tool it calls is missing from the validation environment. This is the host's \
         environment, not a defect in the candidate, so no repair ran, no review or rework \
         budget was spent, and no PR was opened.\n\n- Run: `{run_id}`\n- Failed step: \
         `{failed_step_id}`\n- Candidate branch: `{branch}`\n- Candidate head: `{head_sha}`\n\n\
         Make the tool available to the owner's login shell (or set \
         `workflow.validation_env.path`), check with `orbit doctor`, then resume validation on \
         the same candidate with `orbit job resume {run_id}`.\n\n## Failure\n\n```text\n{}{}\n```",
        &diagnostic[..cut],
        if cut < diagnostic.len() {
            format!("\n[truncated to {cut} of {} bytes]", diagnostic.len())
        } else {
            String::new()
        }
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(VALIDATION_ENVIRONMENT_EVENT.to_string()),
            status_note: Some(note.clone()),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: format!("{note}\n\n{body}"),
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": "blocked_validation_environment",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "candidate_preserved": true,
        "pr_created": false,
        "task_status": "blocked",
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    }))
}

/// Keep the candidate of a run its provider failed [ORB-14266].
///
/// Whatever the agent left is committed on the run's branch, attributed like
/// any failure candidate, but nothing is pushed or published. The task's
/// status is not written here: run finalization moves it to `backlog` under a
/// provider failure hold, for local and PR pipelines alike. The next run
/// resumes this candidate (`candidate_resume` treats the decision as
/// preserving) on a crew the hold permits.
pub(super) fn hold_provider_failure_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let (head_sha, committed_files) = commit_failure_candidate(host, run_id, workspace_path, task)?;
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let class = orbit_types::workflow::ProviderFailureClass::of(None, Some(error_message))
        .map_or("provider_failure", |class| class.as_str());
    Ok(json!({
        "phase": "failure_handoff",
        "decision": "held_provider_failure",
        "provider_failure": class,
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "committed_files": committed_files,
        "candidate_preserved": true,
        "pr_created": false,
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    }))
}

/// Keep a candidate whose required command fails on its base exactly as on
/// the candidate, and hold its task [ORB-14258].
///
/// Like a validation-environment failure, the candidate is a clean committed
/// head the run's branch and worktree already hold: nothing is committed,
/// pushed or published. The task goes back to `backlog` under a
/// [`BASELINE_RED_HOLD_EVENT`](orbit_types::workflow::BASELINE_RED_HOLD_EVENT)
/// whose note carries the hold, so admission withholds it until the base ref
/// moves to a commit where the command may pass. The next run resumes this
/// candidate rather than implementing again.
pub(super) fn hold_baseline_red_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_message: &str,
    hold: &orbit_types::workflow::BaselineRedHold,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let branch = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let diagnostic = error_message.trim();
    let cut = orbit_common::text::floor_char_boundary(
        diagnostic,
        MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES,
    );
    let base_ref = if hold.base_ref.is_empty() {
        "the base".to_string()
    } else {
        format!("`{}`", hold.base_ref)
    };
    let publication = task.github_pr_number().map_or_else(
        || "no PR was opened".to_string(),
        |number| {
            format!(
                "existing PR #{number} remains on its previously published head; this failing \
                 candidate was not pushed"
            )
        },
    );
    let note = hold.text(&format!(
        "required validation `{}` is red on base {}: run={run_id}, failed_step={failed_step_id}, \
         candidate={head_sha}, branch={branch}; held in the backlog until {base_ref} moves to a \
         base where it passes; {publication}",
        hold.command, hold.base_sha
    ));
    let body = format!(
        "## Red base\n\nRequired validation `{}` fails on base `{}` exactly as it fails on this \
         candidate, so the candidate did not cause it. No repair ran and no review or rework budget \
         was spent; {publication}.\n\n- Run: `{run_id}`\n- Failed step: \
         `{failed_step_id}`\n- Candidate branch: `{branch}`\n- Candidate head: `{head_sha}`\n\n\
         The task is back in the backlog and admission skips it while {base_ref} fails \
         the command. Once it passes on a new base tip, the next delivery resumes this candidate and \
         validates it again; fixing the base is the way forward.\n\n## Failure\n\n```text\n{}{}\n```",
        hold.command,
        hold.base_sha,
        &diagnostic[..cut],
        if cut < diagnostic.len() {
            format!("\n[truncated to {cut} of {} bytes]", diagnostic.len())
        } else {
            String::new()
        }
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Backlog),
            status_event: Some(orbit_types::workflow::BASELINE_RED_HOLD_EVENT.to_string()),
            status_note: Some(note),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: body,
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": "held_baseline_red",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": branch,
        "head_sha": head_sha,
        "base_ref": hold.base_ref,
        "base_sha": hold.base_sha,
        "command": hold.command,
        "candidate_preserved": true,
        "pr_created": false,
        "task_status": "backlog",
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    }))
}
