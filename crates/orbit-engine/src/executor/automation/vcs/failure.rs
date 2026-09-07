use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, TaskComment, TaskStatus};
use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string,
};

use super::commit::commit_failure_candidate;
use super::freshness::{commit_sha, original_base_sha, remote_branch_sha};
use super::git::{
    base_sync_mode_from_input, git_command_success, git_output, resolve_worktree_start_point,
};
use super::pr::open_or_reuse_unchecked;
use super::push::push_batch_changes_inner;
use super::resume::ensure_retry_descends_from;

pub(super) use super::resume::commit_head_matches_failure_handoff;

const CONFLICT_BLOCKED_EVENT: &str = "pr_conflict_blocked";
const FAILURE_HANDOFF_EVENT: &str = "pr_failure_handoff";
/// A before-PR review gate stopped delivery [ORB-11333].
const REVIEW_GATE_EVENT: &str = "review_gate_escalation";

/// The pipeline steps that belong to the before-PR review gate.
pub(in crate::executor::automation) const REVIEW_GATE_STEPS: &[&str] =
    &["review_gate_admit", "review", "review_gate_settle"];

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
    if conflicting_paths.is_empty() && rebase_aborted {
        conflicting_paths = conflicts_from_error(error_message);
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
    }))
}

/// Preserve a candidate the before-PR review gate refused to publish.
///
/// Uncommitted reviewer changes are committed under the reviewer identity the
/// gate admitted, never as implementer work; the branch is pushed so partial
/// repairs and evidence are recoverable; the task is blocked with the gate's
/// escalation. No PR is opened.
#[allow(clippy::too_many_arguments)]
fn preserve_review_gate_candidate<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    task: &orbit_types::task::Task,
    run_id: &str,
    failed_step_id: &str,
    error_code: &str,
    error_message: &str,
    workspace_path: &Path,
) -> Result<Value, OrbitError> {
    let reviewer = input
        .get("pipeline")
        .and_then(|pipeline| pipeline.get("review_gate_admit"))
        .and_then(|admit| admit.get("reviewer"));
    let reviewer_model = reviewer.and_then(|reviewer| {
        let provider = reviewer.get("provider")?.as_str()?.trim();
        let model = reviewer.get("model")?.as_str()?.trim();
        (!provider.is_empty() && !model.is_empty()).then(|| format!("{provider} / {model}"))
    });
    let partial_repair = match &reviewer_model {
        Some(model) => super::review_gate::commit_reviewer_repairs(
            workspace_path,
            model,
            &format!(
                "review: partial reviewer repairs preserved [{}]\n\nOrbit-Review-Run: {run_id}\n\
                 Orbit-Review-Step: {failed_step_id}",
                task.id
            ),
        )?,
        None => None,
    };
    let leftover = super::review_gate::uncommitted_paths(workspace_path)?;
    let head = git_output(workspace_path, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    if head == "HEAD" {
        return Err(OrbitError::Execution(
            "pr_failure_handoff: review-gate candidate is detached".to_string(),
        ));
    }
    let head_sha = git_output(workspace_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let pushed = push_batch_changes_inner(
        host,
        &review_gate_preservation_push_input(input, &head, workspace_path, &head_sha)?,
        workspace_path,
    )?;

    let note = format!(
        "before-PR review gate stopped delivery: run={run_id}, failed_step={failed_step_id}, \
         candidate={head_sha}, branch={head}; no PR was opened"
    );
    let body = format!(
        "## Review gate escalation\n\nOrbit held PR publication because the before-PR review gate \
         did not pass. The candidate branch was pushed so the implementation commits, any \
         reviewer repairs, and the review evidence remain inspectable; nothing was merged or \
         published as a PR.\n\n- Task: `{}`\n- Run: `{run_id}`\n- Failed step: `{failed_step_id}`\n\
         - Error code: `{error_code}`\n- Candidate branch: `{head}`\n- Candidate head: `{head_sha}`\n\
         - Partial reviewer repair commit: {}\n- Uncommitted paths left in the worktree: {}\n\n\
         Resuming delivery needs a recorded decision: repair or re-scope, then run the gate again \
         within the lineage's remaining review budget.\n\n## Failure\n\n```text\n{error_message}\n```",
        task.id,
        partial_repair
            .as_ref()
            .map(|commit| format!("`{}` ({})", commit.commit, commit.author))
            .unwrap_or_else(|| "none".to_string()),
        if leftover.is_empty() {
            "none".to_string()
        } else {
            leftover.join(", ")
        },
    );
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            status: Some(TaskStatus::Blocked),
            status_event: Some(REVIEW_GATE_EVENT.to_string()),
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
        "decision": "blocked_review_gate",
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": head,
        "head_sha": head_sha,
        "partial_repair_commit": partial_repair.map(|commit| commit.commit),
        "uncommitted_paths": leftover,
        "push": pushed,
        "pr_created": false,
        "task_status": "blocked",
    }))
}

/// Push input for a review-gate preservation.
///
/// First-time (missing origin) and fast-forward pushes ignore the lease
/// fields. A diverged origin is replaced only when the lease names the exact
/// remote SHA `git_push` currently observes. `rewrite_performed` is set by
/// this handoff even when this run's `sync_base` did not rewrite: a previous
/// preservation (or re-implementation onto the same head) still has to replace
/// the published candidate.
fn review_gate_preservation_push_input(
    input: &Value,
    branch: &str,
    workspace_path: &Path,
    local_sha: &str,
) -> Result<Value, OrbitError> {
    let mut push_input = json!({
        "branch": branch,
        "workspace_path": workspace_path,
    });
    if let Some((head_before, expected_remote_sha)) =
        review_gate_rewrite_lease(input, branch, workspace_path, local_sha)?
    {
        push_input["rewrite_performed"] = json!(true);
        push_input["rewrite_head_before"] = json!(head_before);
        push_input["expected_remote_sha"] = json!(expected_remote_sha);
    }
    Ok(push_input)
}

fn review_gate_rewrite_lease(
    input: &Value,
    branch: &str,
    workspace_path: &Path,
    local_sha: &str,
) -> Result<Option<(String, String)>, OrbitError> {
    let checkpointed_remote = pipeline_checkpoint_string(input, "sync_base", "remote_sha_before")
        .or_else(|| pipeline_checkpoint_string(input, "prepare_branch", "remote_sha"));
    let expected_remote_sha = match checkpointed_remote {
        Some(sha) => sha,
        None => match remote_branch_sha(workspace_path, branch)? {
            Some(sha) => sha,
            None => return Ok(None),
        },
    };

    let head_before = pipeline_checkpoint_string(input, "sync_base", "head_sha_before")
        .filter(|sha| sha != local_sha)
        .or_else(|| (expected_remote_sha != local_sha).then(|| expected_remote_sha.clone()));
    Ok(head_before.map(|head_before| (head_before, expected_remote_sha)))
}

fn pipeline_checkpoint_string(input: &Value, step: &str, field: &str) -> Option<String> {
    input
        .get("pipeline")
        .and_then(|pipeline| pipeline.get(step))
        .and_then(|checkpoint| input_string_field(checkpoint, field))
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
