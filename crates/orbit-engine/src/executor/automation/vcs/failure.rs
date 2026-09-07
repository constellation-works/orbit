use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, TaskComment, TaskStatus};
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

use crate::DispatchError;
use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string,
};

use super::commit::commit_failure_candidate;
use super::freshness::{commit_sha, original_base_sha};
use super::git::{
    base_sync_mode_from_input, git_command_success, git_output, resolve_worktree_start_point,
};
use super::pr::open_or_reuse_unchecked;
use super::push::push_batch_changes_inner;

const CONFLICT_BLOCKED_EVENT: &str = "pr_conflict_blocked";
const FAILURE_HANDOFF_EVENT: &str = "pr_failure_handoff";
const FAILURE_HANDOFF_LINEAGE_MAX_HOPS: usize = 64;

const RESUME_PRESERVATION_ERROR: &str = "resume_preservation_unverified";

/// Authenticate a terminal failure handoff before a resumed run executes the
/// first unfinished step.
///
/// The successful worktree checkpoint intentionally retains its original
/// `base_sha`. If HEAD still equals that commit there is nothing to reconcile.
/// A moved HEAD is accepted only when the source run durably recorded an exact
/// `pr_failure_handoff` result for the same task, checkpoint owner, and commit,
/// and the active run descends from the handoff run. The evidence stays in the
/// durable run state for the later commit gate; no historical checkpoint or
/// repository state is rewritten.
pub(crate) fn reconcile_resumed_failure_handoff(
    host: &dyn RuntimeHost,
    job: &JobV2,
    active_run_id: &str,
    resume: &PipelineState,
    pipeline: &HashMap<String, Value>,
) -> Result<(), DispatchError> {
    let Some(commit_index) = job
        .steps
        .iter()
        .position(|step| step.id == "commit")
        .map(|index| index as u32)
    else {
        return Ok(());
    };
    if resume.step_states.get(&commit_index) == Some(&JobRunState::Success) {
        return Ok(());
    }

    let Some(worktree) = pipeline.get("worktree") else {
        return Ok(());
    };
    let Some(workspace_path) = input_string_field(worktree, "workspace_path") else {
        return Ok(());
    };
    let Some(original_base_sha) = input_string_field(worktree, "base_sha") else {
        return Ok(());
    };
    let Some(checkpoint_owner) = input_string_field(worktree, "job_run_id")
        .or_else(|| input_string_field(worktree, "batch_id"))
    else {
        return Ok(());
    };

    let workspace_path = canonicalize_existing_dir(&workspace_path, "resume workspace_path")
        .map_err(resume_preservation_error)?;
    let base_sha =
        commit_sha(&workspace_path, &original_base_sha).map_err(resume_preservation_error)?;
    let head_sha = commit_sha(&workspace_path, "HEAD").map_err(resume_preservation_error)?;
    if head_sha == base_sha {
        return Ok(());
    }

    let checkpoint = resume.failure_activity_checkpoint.as_ref().ok_or_else(|| {
        resume_preservation_error(OrbitError::Execution(format!(
            "resume found HEAD {head_sha} past immutable worktree base {base_sha}, but source run '{}' has no durable failure-activity preservation evidence",
            resume.run_id
        )))
    })?;
    let evidence = validate_failure_handoff_evidence(
        host,
        active_run_id,
        &checkpoint_owner,
        &workspace_path,
        &base_sha,
        &head_sha,
        checkpoint,
    )
    .map_err(resume_preservation_error)?;
    let evidence_task_id =
        required_input_string(evidence, "task_id").map_err(resume_preservation_error)?;
    let targets_task = resume
        .initial_input
        .get("task_ids")
        .and_then(Value::as_array)
        .is_some_and(|task_ids| {
            task_ids
                .iter()
                .any(|task_id| task_id.as_str() == Some(evidence_task_id))
        });
    if !targets_task {
        return Err(resume_preservation_error(OrbitError::Execution(format!(
            "failure handoff evidence belongs to task '{evidence_task_id}', which is not targeted by the resumed run"
        ))));
    }
    Ok(())
}

/// Re-authenticate the evidence carried into `git_commit` against the source
/// run's immutable state. This is deliberately separate from the ordinary
/// moved-HEAD escape hatch used by epic child merges.
pub(super) fn commit_head_matches_failure_handoff<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task: &orbit_types::task::Task,
    checkpoint_owner: &str,
    workspace_path: &Path,
    base_sha: &str,
    head_sha: &str,
) -> Result<bool, OrbitError> {
    let Some(active_run_id) = input_string_field(input, "run_id") else {
        return Ok(false);
    };
    let Some(active_state) = host.read_run_state(&active_run_id)? else {
        return Ok(false);
    };
    let Some(checkpoint) = active_state.failure_activity_checkpoint.as_ref() else {
        return Ok(false);
    };
    let evidence = &checkpoint.output;
    validate_failure_handoff_evidence(
        host,
        &active_run_id,
        checkpoint_owner,
        workspace_path,
        base_sha,
        head_sha,
        checkpoint,
    )?;
    if evidence.get("task_id").and_then(Value::as_str) != Some(task.id.as_str()) {
        return Err(OrbitError::Execution(format!(
            "git_commit: failure handoff evidence belongs to a different task than '{}'",
            task.id
        )));
    }
    Ok(true)
}

fn validate_failure_handoff_evidence<'a, H: RuntimeHost + ?Sized>(
    host: &H,
    active_run_id: &str,
    checkpoint_owner: &str,
    workspace_path: &Path,
    base_sha: &str,
    head_sha: &str,
    checkpoint: &'a orbit_types::workflow::FailureActivityCheckpoint,
) -> Result<&'a Value, OrbitError> {
    if checkpoint.activity_name != "pr_failure_handoff" {
        return Err(OrbitError::Execution(format!(
            "failure activity '{}' is not authorized to reconcile a resumed worktree HEAD",
            checkpoint.activity_name
        )));
    }
    let evidence = &checkpoint.output;
    let phase = required_input_string(evidence, "phase")?;
    let decision = required_input_string(evidence, "decision")?;
    if phase != "failure_handoff"
        || !matches!(decision, "blocked_failure_pr" | "blocked_conflict_pr")
    {
        return Err(OrbitError::Execution(format!(
            "failure activity result '{phase}/{decision}' is not candidate-preservation evidence"
        )));
    }

    let task_id = required_input_string(evidence, "task_id")?;
    let handoff_run_id = required_input_string(evidence, "handoff_run_id")?;
    let evidence_owner = required_input_string(evidence, "checkpoint_owner")?;
    let evidence_base = required_input_string(evidence, "original_base_sha")?;
    let evidence_head = required_input_string(evidence, "head_sha")?;
    if evidence
        .get("preservation_commit_created")
        .and_then(Value::as_bool)
        != Some(true)
    {
        return Err(OrbitError::Execution(
            "failure handoff did not create the candidate commit at the recorded HEAD".to_string(),
        ));
    }
    if evidence_owner != checkpoint_owner {
        return Err(OrbitError::Execution(format!(
            "failure handoff checkpoint owner '{evidence_owner}' does not match reused worktree owner '{checkpoint_owner}'"
        )));
    }
    if evidence_base != base_sha || evidence_head != head_sha {
        return Err(OrbitError::Execution(format!(
            "failure handoff evidence expected base {evidence_base} and HEAD {evidence_head}, but resume observed base {base_sha} and HEAD {head_sha}"
        )));
    }

    let source_state = host.read_run_state(handoff_run_id)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "failure handoff run '{handoff_run_id}' has no durable run state"
        ))
    })?;
    if source_state.failure_activity_checkpoint.as_ref() != Some(checkpoint) {
        return Err(OrbitError::Execution(format!(
            "failure handoff evidence does not match the immutable state of run '{handoff_run_id}'"
        )));
    }
    ensure_preservation_parent_owned(
        host,
        workspace_path,
        handoff_run_id,
        base_sha,
        head_sha,
        &source_state,
    )?;

    let task = host.get_task(task_id)?;
    if task.job_run_id.as_deref() != Some(checkpoint_owner) {
        return Err(OrbitError::Execution(format!(
            "task '{task_id}' belongs to run '{}', not preserved worktree owner '{checkpoint_owner}'",
            task.job_run_id.as_deref().unwrap_or("none")
        )));
    }
    ensure_retry_descends_from(
        host,
        "resume preservation",
        "failure handoff run",
        task_id,
        active_run_id,
        handoff_run_id,
    )?;
    Ok(evidence)
}

fn ensure_preservation_parent_owned<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &Path,
    handoff_run_id: &str,
    base_sha: &str,
    head_sha: &str,
    handoff_state: &PipelineState,
) -> Result<(), OrbitError> {
    let parent_sha = commit_sha(workspace_path, &format!("{head_sha}^"))?;
    if parent_sha == base_sha
        || handoff_state.step_outputs.values().any(|output| {
            output.get("commit_sha").and_then(Value::as_str) == Some(parent_sha.as_str())
        })
    {
        return Ok(());
    }

    let handoff_run = host.get_job_run(handoff_run_id)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "failure handoff run '{handoff_run_id}' was not found while verifying preservation ancestry"
        ))
    })?;
    let mut cursor = handoff_run.retry_source_run_id;
    for _ in 0..FAILURE_HANDOFF_LINEAGE_MAX_HOPS {
        let Some(run_id) = cursor.take() else { break };
        let run = host.get_job_run(&run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "retry ancestor '{run_id}' was not found while verifying preservation ancestry"
            ))
        })?;
        if run.job_id != handoff_run.job_id {
            return Err(OrbitError::Execution(format!(
                "preservation ancestry crosses from job '{}' to job '{}' at run '{}'",
                handoff_run.job_id, run.job_id, run.run_id
            )));
        }
        let ancestor_head = host
            .read_run_state(&run_id)?
            .and_then(|state| state.failure_activity_checkpoint)
            .and_then(|checkpoint| checkpoint.output.get("head_sha").cloned())
            .and_then(|head| head.as_str().map(ToOwned::to_owned));
        if ancestor_head.as_deref() == Some(parent_sha.as_str()) {
            return Ok(());
        }
        if run.retry_source_run_id.as_deref() == Some(run_id.as_str()) {
            break;
        }
        cursor = run.retry_source_run_id;
    }

    Err(OrbitError::Execution(format!(
        "failure handoff commit {head_sha} has unowned parent {parent_sha}; expected immutable base {base_sha}, a successful workflow commit, or an earlier preservation commit in the retry lineage"
    )))
}

fn resume_preservation_error(error: OrbitError) -> DispatchError {
    DispatchError::WorktreeIntegrity {
        code: RESUME_PRESERVATION_ERROR,
        diagnostic: error.to_string(),
    }
}

/// Terminal hook for `task_pr_pipeline`.
///
/// The original job error remains authoritative. Failures before publication
/// make the candidate recoverable and block the task for reconciliation.
/// Completion failures after publication preserve that exact PR and keep the
/// task in review so an operator can fix the named merge gate and safely retry.
pub(in crate::executor::automation) fn pr_failure_handoff<H: RuntimeHost + Sync + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let failed_step_id = required_input_string(input, "failed_step_id")?;
    let error_code = required_input_string(input, "error_code")?;
    let error_message = required_input_string(input, "error_message")?;
    let run_id = required_input_string(input, "run_id")?;
    let job_input = input
        .get("job_input")
        .ok_or_else(|| OrbitError::InvalidInput("missing required input.job_input".to_string()))?;
    let task_ids = job_input
        .get("task_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "pr_failure_handoff: job_input.task_ids must be an array".to_string(),
            )
        })?;
    let [task_id] = task_ids.as_slice() else {
        return Err(OrbitError::InvalidInput(format!(
            "pr_failure_handoff expected exactly one task id, got {}",
            task_ids.len()
        )));
    };
    let task_id = task_id.as_str().ok_or_else(|| {
        OrbitError::InvalidInput(
            "pr_failure_handoff: task id must be a non-empty string".to_string(),
        )
    })?;
    let task = host.get_task(task_id)?;
    ensure_failure_handoff_ownership(host, input, &task, run_id)?;

    if failed_step_id == "complete_pr"
        && let Some(pr_number) = task.github_pr_number().map(ToOwned::to_owned)
    {
        return preserve_completion_failure(
            host,
            &task,
            run_id,
            failed_step_id,
            error_code,
            error_message,
            &pr_number,
        );
    }

    let worktree = pipeline_step(input, "worktree")?;
    let checkpoint_owner = input_string_field(worktree, "job_run_id")
        .or_else(|| input_string_field(worktree, "batch_id"))
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "pr_failure_handoff: run '{run_id}' has no worktree ownership checkpoint"
            ))
        })?;
    let workspace_path = canonicalize_existing_dir(
        required_input_string(worktree, "workspace_path")?,
        "pipeline.worktree.workspace_path",
    )?;

    let mut conflicting_paths = unmerged_paths(&workspace_path)?;
    let rebase_aborted = git_command_success(&workspace_path, &["rebase", "--abort"])?;
    if !conflicting_paths.is_empty() && !rebase_aborted {
        return Err(OrbitError::Execution(
            "pr_failure_handoff: conflicts exist but the in-progress rebase could not be aborted"
                .to_string(),
        ));
    }
    if conflicting_paths.is_empty() {
        conflicting_paths = conflicts_from_error(error_message);
    }

    let (head_sha, committed_files) =
        commit_failure_candidate(host, run_id, &workspace_path, &task)?;
    let head = git_output(&workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if head == "HEAD" {
        return Err(OrbitError::Execution(
            "pr_failure_handoff: recovery candidate is detached".to_string(),
        ));
    }

    let base = input_string_field(job_input, "base_branch").unwrap_or_else(|| "main".to_string());
    let target_base_sha = match prepared_base_sha(input) {
        Some(base_sha) => base_sha,
        None => {
            let sync_mode = base_sync_mode_from_input(job_input)?;
            let target_base_ref = resolve_worktree_start_point(&workspace_path, &base, sync_mode)?;
            commit_sha(&workspace_path, &target_base_ref)?
        }
    };
    let original_base_sha = original_base_sha(&workspace_path, &head_sha, &target_base_sha)?;

    if committed_files.is_empty() && head_sha == original_base_sha {
        return Ok(json!({
            "phase": "failure_handoff",
            "decision": "no_candidate",
            "failed_step_id": failed_step_id,
            "workspace_path": workspace_path,
        }));
    }

    let pushed = push_batch_changes_inner(
        host,
        &json!({
            "branch": head,
            "workspace_path": workspace_path,
        }),
        &workspace_path,
    )?;
    let title = input_string_field(job_input, "title")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("[BLOCKED] {}", task.title.trim()));
    let body = blocked_pr_body(
        &task.id,
        run_id,
        failed_step_id,
        error_code,
        error_message,
        &original_base_sha,
        &target_base_sha,
        &conflicting_paths,
    );
    let (pr_number, pr_url, pr_created) =
        open_or_reuse_unchecked(host, &workspace_path, &head, &base, &title, &body)?;

    let conflict_blocked = !conflicting_paths.is_empty();
    let event = if conflict_blocked {
        CONFLICT_BLOCKED_EVENT
    } else {
        FAILURE_HANDOFF_EVENT
    };
    let paths = if conflicting_paths.is_empty() {
        "none reported".to_string()
    } else {
        conflicting_paths.join(", ")
    };
    let note = format!(
        "failure handoff published PR #{pr_number}: run={run_id}, failed_step={failed_step_id}, \
         original_base={original_base_sha}, target_base={target_base_sha}, conflicts={paths}"
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(event.to_string()),
            status_note: Some(note.clone()),
            external_refs: vec![ExternalRef::github_pr(pr_number.clone())?],
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
        "decision": if conflict_blocked { "blocked_conflict_pr" } else { "blocked_failure_pr" },
        "task_id": task.id,
        "handoff_run_id": run_id,
        "checkpoint_owner": checkpoint_owner,
        "preservation_commit_created": !committed_files.is_empty(),
        "failed_step_id": failed_step_id,
        "branch": head,
        "head_sha": head_sha,
        "original_base_sha": original_base_sha,
        "target_base_sha": target_base_sha,
        "conflicting_paths": conflicting_paths,
        "committed_files": committed_files,
        "push": pushed,
        "pr_number": pr_number,
        "pr_url": pr_url,
        "pr_created": pr_created,
        "task_status": "blocked",
    }))
}

fn ensure_failure_handoff_ownership<H: RuntimeHost + ?Sized>(
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

fn ensure_retry_descends_from<H: RuntimeHost + ?Sized>(
    host: &H,
    operation: &str,
    ancestor_label: &str,
    task_id: &str,
    run_id: &str,
    ancestor_run_id: &str,
) -> Result<(), OrbitError> {
    let mut current = host.get_job_run(run_id)?.ok_or_else(|| {
        OrbitError::Execution(format!(
            "{operation}: cannot verify ownership for task '{task_id}'; active run '{run_id}' was not found"
        ))
    })?;
    let job_id = current.job_id.clone();

    for _ in 0..FAILURE_HANDOFF_LINEAGE_MAX_HOPS {
        let Some(parent_run_id) = current.retry_source_run_id.as_deref() else {
            return Err(OrbitError::Execution(format!(
                "{operation}: run '{run_id}' is not a retry descendant of {ancestor_label} '{ancestor_run_id}' for task '{task_id}'"
            )));
        };
        let parent = host.get_job_run(parent_run_id)?.ok_or_else(|| {
            OrbitError::Execution(format!(
                "{operation}: cannot verify ownership for task '{task_id}'; retry ancestor '{parent_run_id}' was not found"
            ))
        })?;
        if parent.job_id != job_id {
            return Err(OrbitError::Execution(format!(
                "{operation}: retry lineage for run '{run_id}' crosses from job '{job_id}' to job '{}' at run '{}'; refusing handoff for task '{task_id}'",
                parent.job_id, parent.run_id
            )));
        }
        if parent.run_id == ancestor_run_id {
            return Ok(());
        }
        if parent.run_id == current.run_id {
            break;
        }
        current = parent;
    }

    Err(OrbitError::Execution(format!(
        "{operation}: run '{run_id}' has no bounded retry lineage to {ancestor_label} '{ancestor_run_id}' for task '{task_id}'"
    )))
}

#[allow(clippy::too_many_arguments)]
fn preserve_completion_failure<H: RuntimeHost + ?Sized>(
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
    let note = format!(
        "PR completion failed after publication; preserved PR #{pr_number} and task status '{}' \
         for a safe completion retry. No candidate, PR body, branch, or repository setting was \
         changed.\n\n- Run: `{run_id}`\n- Failed step: `{failed_step_id}`\n- Error code: \
         `{error_code}`\n\nFailure:\n```text\n{error_message}\n```",
        task.status
    );
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
        "decision": "review_completion_failure",
        "failed_step_id": failed_step_id,
        "pr_number": pr_number,
        "pr_url": pr_url,
        "candidate_preserved": true,
        "task_status": task.status.to_string(),
    }))
}

fn pipeline_step<'a>(input: &'a Value, step: &str) -> Result<&'a Value, OrbitError> {
    input
        .get("pipeline")
        .and_then(|pipeline| pipeline.get(step))
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "pr_failure_handoff: missing pipeline.{step} checkpoint"
            ))
        })
}

fn prepared_base_sha(input: &Value) -> Option<String> {
    input
        .get("pipeline")
        .and_then(|pipeline| pipeline.get("prepare_branch"))
        .and_then(|prepare| prepare.get("base_sha"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|sha| !sha.is_empty())
        .map(ToOwned::to_owned)
}

fn unmerged_paths(workspace_path: &Path) -> Result<Vec<String>, OrbitError> {
    Ok(
        git_output(workspace_path, &["diff", "--name-only", "--diff-filter=U"])?
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
    )
}

fn conflicts_from_error(error: &str) -> Vec<String> {
    let Some((_, paths)) = error.split_once("conflicting paths: ") else {
        return Vec::new();
    };
    paths
        .lines()
        .next()
        .unwrap_or_default()
        .split(", ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn blocked_pr_body(
    task_id: &str,
    run_id: &str,
    failed_step_id: &str,
    error_code: &str,
    error_message: &str,
    original_base_sha: &str,
    target_base_sha: &str,
    conflicting_paths: &[String],
) -> String {
    let (heading, summary) = if conflicting_paths.is_empty() {
        (
            "Delivery failure handoff",
            "Orbit preserved and pushed this task's candidate after the shipment pipeline failed. \
             This PR is intentionally blocked; inspect the recorded failure before retrying delivery.",
        )
    } else {
        (
            "Merge conflict handoff",
            "Orbit preserved and pushed this task's candidate after a merge conflict stopped delivery. \
             This PR is intentionally blocked; reconcile the named paths before retrying delivery.",
        )
    };
    let conflicts = if conflicting_paths.is_empty() {
        "- None reported; inspect the failed pipeline step before merging.".to_string()
    } else {
        conflicting_paths
            .iter()
            .map(|path| format!("- `{path}`"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "## {heading}\n\n{summary}\n\n\
         - Task: `{task_id}`\n\
         - Run: `{run_id}`\n\
         - Failed step: `{failed_step_id}`\n\
         - Error code: `{error_code}`\n\
         - Original base: `{original_base_sha}`\n\
         - Target base: `{target_base_sha}`\n\n\
         ## Conflicting paths\n\n{conflicts}\n\n\
         ## Failure\n\n```text\n{error_message}\n```"
    )
}
