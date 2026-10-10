use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, TaskComment, TaskStatus};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string,
};

use super::super::commit::commit_failure_candidate;
use super::super::freshness::{commit_sha, original_base_sha};
use super::super::git::{
    base_sync_mode_from_input, git_command_success, git_output, resolve_worktree_start_point,
};
use super::super::handoff::rebase_in_progress;
use super::super::pr::open_or_reuse_unchecked;
use super::super::push::push_batch_changes_inner;
use super::conflict::{
    blocked_pr_body, conflicts_from_error, pipeline_step, prepared_base_sha, unmerged_paths,
};
use super::ownership::{
    ensure_failure_handoff_ownership, prepared_attempt_owns_rebase, preserve_completion_failure,
    refuse_foreign_rebase, release_review_attempts,
};
use super::preserve::{
    hold_baseline_red_candidate, hold_provider_failure_candidate, preserve_agent_blocked_candidate,
    preserve_validation_environment_candidate, recorded_spec_digest,
};
use super::review_gate::preserve_review_gate_candidate;
use super::{COMPLETION_STEPS, CONFLICT_BLOCKED_EVENT, FAILURE_HANDOFF_EVENT, REVIEW_GATE_STEPS};

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
    // Before anything that may refuse the handoff — a bundle, a missing
    // task — so every attempt this run admitted is closed.
    release_review_attempts(host, input, run_id);
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

    // [ORB-14258] Completion failures normally preserve their published PR,
    // but a required check that the base shares is not a candidate failure.
    // Handle it before that preservation path so the task is held with the
    // typed base condition instead of left in review indefinitely.
    if orbit_types::workflow::is_baseline_red_failure(Some(error_code), Some(error_message))
        && let Some(hold) = orbit_types::workflow::BaselineRedHold::from_text(error_message)
    {
        let worktree = pipeline_step(input, "worktree")?;
        let workspace_path = canonicalize_existing_dir(
            required_input_string(worktree, "workspace_path")?,
            "pipeline.worktree.workspace_path",
        )?;
        return hold_baseline_red_candidate(
            host,
            &task,
            run_id,
            failed_step_id,
            error_message,
            &hold,
            &workspace_path,
        );
    }

    if COMPLETION_STEPS.contains(&failed_step_id)
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

    // [ORB-14269] The implementer, or step recovery [ORB-14268], declared a
    // blocker. The worktree may be dirty. Leave it: do not abort a rebase, commit, push,
    // or open a `[BLOCKED]` PR. Checked before those mutations.
    if orbit_types::workflow::is_task_blocked_by_agent(Some(error_code), Some(error_message)) {
        return preserve_agent_blocked_candidate(
            host,
            &task,
            run_id,
            failed_step_id,
            error_message,
            &workspace_path,
        );
    }

    let mut conflicting_paths = unmerged_paths(&workspace_path)?;
    // [ORB-13455] Task and run ownership do not prove this run started the
    // Git rebase. Only the rebase `sync_base` started from the prepared
    // checkpoint may be aborted; any other rebase is left exactly as found.
    let rebase_aborted = if rebase_in_progress(&workspace_path)? {
        if !prepared_attempt_owns_rebase(input, &workspace_path)? {
            return refuse_foreign_rebase(
                host,
                &task,
                run_id,
                failed_step_id,
                error_code,
                error_message,
                &workspace_path,
            );
        }
        git_command_success(&workspace_path, &["rebase", "--abort"])?
    } else {
        false
    };
    if !conflicting_paths.is_empty() && !rebase_aborted {
        return Err(OrbitError::Execution(
            "pr_failure_handoff: conflicts exist but the in-progress rebase could not be aborted"
                .to_string(),
        ));
    }
    if conflicting_paths.is_empty() && rebase_aborted {
        conflicting_paths = conflicts_from_error(error_message);
    }

    // [ORB-13987] Required validation lacked a tool. Nothing about the
    // candidate is known, so it is neither published as a `[BLOCKED]` PR nor
    // repaired: it stays exactly as validated for `orbit job resume`. Checked
    // before the review-gate branch because `rework_validate` fails inside
    // that loop.
    if orbit_types::workflow::is_validation_environment_failure(
        Some(error_code),
        Some(error_message),
    ) {
        return preserve_validation_environment_candidate(
            host,
            &task,
            run_id,
            failed_step_id,
            error_message,
            &workspace_path,
        );
    }

    // [ORB-14266] The provider failed the run, not the candidate: capacity,
    // an unusable provider, or a content-policy refusal. Commit what the
    // agent left so the next run resumes it, open no `[BLOCKED]` PR, and
    // leave the task's status to run finalization, which holds it in the
    // backlog with the failing provider's crews excluded. Checked before the
    // review-gate branch because the reviewer's provider can fail too.
    if orbit_types::workflow::is_provider_failure(Some(error_code), Some(error_message)) {
        return hold_provider_failure_candidate(
            host,
            &task,
            run_id,
            failed_step_id,
            error_message,
            &workspace_path,
        );
    }

    // [ORB-11333] A review-gate failure keeps the implementation and any
    // partial reviewer repairs attributed to their authors, pushes the
    // candidate so the evidence survives, and opens no PR: publication is
    // exactly what the gate withheld.
    if REVIEW_GATE_STEPS.contains(&failed_step_id) {
        return preserve_review_gate_candidate(
            host,
            input,
            &task,
            run_id,
            failed_step_id,
            error_code,
            error_message,
            &workspace_path,
        );
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
        // Resume authenticates against the immutable worktree base, even when
        // a recovered rebase moved the candidate's merge base forward.
        "original_base_sha": input_string_field(worktree, "base_sha").unwrap_or(original_base_sha),
        "target_base_sha": target_base_sha,
        "conflicting_paths": conflicting_paths,
        "committed_files": committed_files,
        "push": pushed,
        "pr_number": pr_number,
        "pr_url": pr_url,
        "pr_created": pr_created,
        "task_status": "blocked",
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
    }))
}
