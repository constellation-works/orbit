use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{ExternalRef, TaskComment, TaskStatus};
use serde_json::{Value, json};

use crate::context::{ReviewReleaseRequest, RuntimeHost, TaskAutomationUpdate};
use crate::executor::automation::input::{
    canonicalize_existing_dir, input_string_field, required_input_string,
};

use super::commit::commit_failure_candidate;
use super::freshness::{
    commit_sha, original_base_sha, rebase_belongs_to_attempt, rebase_provenance_summary,
    remote_branch_sha,
};
use super::git::{
    base_sync_mode_from_input, git_command_success, git_output, resolve_worktree_start_point,
};
use super::handoff::rebase_in_progress;
use super::pr::open_or_reuse_unchecked;
use super::push::push_batch_changes_inner;
use super::resume::ensure_retry_descends_from;

pub(super) use super::resume::commit_head_matches_failure_handoff;

const CONFLICT_BLOCKED_EVENT: &str = "pr_conflict_blocked";
const FAILURE_HANDOFF_EVENT: &str = "pr_failure_handoff";
/// The handoff found a rebase this run did not start and left it intact [ORB-13455].
const FOREIGN_REBASE_EVENT: &str = "pr_foreign_rebase_refused";
/// A before-PR review gate stopped delivery [ORB-11333].
const REVIEW_GATE_EVENT: &str = "review_gate_escalation";
/// Required validation could not find a tool; the candidate was not judged
/// [ORB-13987].
const VALIDATION_ENVIRONMENT_EVENT: &str = "validation_environment_blocked";
/// Largest validation diagnostic the blocking comment repeats; the full
/// output is in the attached validation log.
const MAX_VALIDATION_ENVIRONMENT_DIAGNOSTIC_BYTES: usize = 16 * 1024;

/// The pipeline steps that belong to the before-PR review gate: admission,
/// the reviewer, settlement, and owner revalidation of the reviewer's fixes
/// [ORB-13989].
pub(in crate::executor::automation) const REVIEW_GATE_STEPS: &[&str] = &[
    "review_gate_admit",
    "review",
    "review_gate_settle",
    REVIEW_VALIDATION_STEP,
];

/// Owner revalidation of the reviewer commit. Its failure rejects the
/// candidate: there is no second review round [ORB-13989].
const REVIEW_VALIDATION_STEP: &str = "review_validate";

/// Completion-stage steps: merging the published PR, and re-reviewing and
/// republishing it after completion rebased a conflicting reviewed head —
/// up to two rounds, when the base moved again during the first.
const COMPLETION_STEPS: &[&str] = &[
    "complete_pr",
    "re_review_gate_admit",
    "re_review",
    "re_review_gate_settle",
    "re_review_validate",
    "re_push",
    "complete_reviewed_pr",
    "re_review_gate_admit_2",
    "re_review_2",
    "re_review_gate_settle_2",
    "re_review_validate_2",
    "re_push_2",
    "complete_reviewed_pr_2",
];

/// Admission checkpoints whose attempt a failing run must close.
const REVIEW_ADMISSION_STEPS: &[&str] = &[
    "review_gate_admit",
    "re_review_gate_admit",
    "re_review_gate_admit_2",
];

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

    // [ORB-14269] The implementer declared a blocker before commit. The
    // worktree may be dirty. Leave it: do not abort a rebase, commit, push,
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

/// The spec digest a later run compares before resuming this candidate
/// [ORB-13985], read after this handoff's own selector widening.
fn recorded_spec_digest<H: RuntimeHost + ?Sized>(
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
fn preserve_agent_blocked_candidate<H: RuntimeHost + ?Sized>(
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
fn preserve_validation_environment_candidate<H: RuntimeHost + ?Sized>(
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
fn hold_provider_failure_candidate<H: RuntimeHost + ?Sized>(
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
fn hold_baseline_red_candidate<H: RuntimeHost + ?Sized>(
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

/// Preserve a candidate the before-PR review gate refused to publish.
///
/// Uncommitted reviewer changes are committed under the reviewer identity the
/// gate admitted, never as implementer work; the implementation and reviewer
/// commits stay as they are; the branch is pushed so partial fixes and
/// evidence are recoverable; the task is blocked with the gate's escalation.
/// No PR is opened.
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
    let attempt_id = input
        .get("pipeline")
        .and_then(|pipeline| pipeline.get("review_gate_admit"))
        .and_then(|admit| input_string_field(admit, "attempt_id"));
    let attempt_trailer = attempt_id
        .map(|id| format!("\nOrbit-Review-Attempt: {id}"))
        .unwrap_or_default();
    let partial_repair = match &reviewer_model {
        Some(model) => super::review_gate::commit_reviewer_repairs(
            workspace_path,
            model,
            &format!(
                "review: partial reviewer repairs preserved [{}]\n\nOrbit-Review-Run: {run_id}\n\
                 Orbit-Review-Step: {failed_step_id}{attempt_trailer}",
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

    let evidence_hold = task.status == TaskStatus::InProgress
        && failed_step_id == "review_gate_settle"
        && error_message.contains("review_awaiting_evidence:")
        && host.get_task_artifacts(&task.id)?.iter().any(|artifact| {
            let admitted = input
                .get("pipeline")
                .and_then(|pipeline| pipeline.get("review_gate_admit"));
            let attempt_id = admitted.and_then(|admit| input_string_field(admit, "attempt_id"));
            let lineage_key = admitted.and_then(|admit| input_string_field(admit, "lineage_key"));
            artifact.path == orbit_types::workflow::REVIEW_EVIDENCE_HOLD_ARTIFACT
                && artifact.created_by.as_deref() == Some("system")
                && serde_json::from_slice::<orbit_types::workflow::ReviewEvidenceHold>(
                    &artifact.content,
                )
                .is_ok_and(|hold| {
                    !hold.requirements.is_empty()
                        && attempt_id.as_deref() == Some(hold.attempt_id.as_str())
                        && lineage_key.as_deref() == Some(hold.lineage_key.as_str())
                        && hold.run_id == run_id
                        && hold.candidate.commit == head_sha
                })
        });
    let timed_out = task.status == TaskStatus::InProgress
        && failed_step_id == "review"
        && error_message.contains("review_timeout_incomplete:");
    let (status, event, decision) = if evidence_hold {
        (
            TaskStatus::InProgress,
            "review_awaiting_evidence",
            "awaiting_review_evidence",
        )
    } else if timed_out {
        (
            TaskStatus::Backlog,
            "review_timeout_incomplete",
            "incomplete_review_timeout",
        )
    } else {
        (
            TaskStatus::Blocked,
            REVIEW_GATE_EVENT,
            "blocked_review_gate",
        )
    };
    let note = format!(
        "before-PR review gate stopped delivery: run={run_id}, failed_step={failed_step_id}, \
         candidate={head_sha}, branch={head}; no PR was opened"
    );
    let verdict = if failed_step_id == REVIEW_VALIDATION_STEP {
        "Verdict: `reject`. The reviewer's fixes did not pass owner revalidation (required \
         validation or path ownership) on the reviewed head, and there is no second review \
         round.\n\n"
    } else {
        ""
    };
    let body = format!(
        "## Review gate escalation\n\nOrbit held PR publication because the before-PR review gate \
         did not pass. {verdict}The candidate branch was pushed so the implementation commit, \
         any reviewer commit, and the review evidence remain inspectable; nothing was merged or \
         published as a PR.\n\n- Task: `{}`\n- Run: `{run_id}`\n- Failed step: `{failed_step_id}`\n\
         - Error code: `{error_code}`\n- Candidate branch: `{head}`\n- Candidate head: `{head_sha}`\n\
         - Partial reviewer commit: {}\n\
         - Uncommitted paths left in the worktree: {}\n\n\
         The review's verdict, findings and what changed for each are recorded in the gate's \
         settlement comment on the task. Continuation follows the timeout, evidence, or \
         substantive decision recorded with this handoff.\n\n\
         ## Failure\n\n```text\n{error_message}\n```",
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
    let continuation = if evidence_hold {
        "Awaiting named external evidence. Attach each matching passing result and its log to the task; once all checks arrive, delivery is requeued for review."
    } else if timed_out {
        "Reviewer timed out and settled incomplete. The partial report is retained; delivery is requeued to continue the review within the remaining minute budget."
    } else {
        "A substantive review escalation requires a recorded repair or scope decision."
    };
    host.apply_task_automation_update(
        &task.id,
        TaskAutomationUpdate {
            expected_status: Some(task.status),
            status: Some(status),
            status_event: Some(event.to_string()),
            status_note: Some(note.clone()),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: format!("{note}\n\n{continuation}\n\n{body}"),
            }],
            ..TaskAutomationUpdate::default()
        },
    )?;

    Ok(json!({
        "phase": "failure_handoff",
        "decision": decision,
        "task_id": task.id,
        "handoff_run_id": run_id,
        "failed_step_id": failed_step_id,
        "branch": head,
        "head_sha": head_sha,
        "partial_repair_commit": partial_repair.map(|commit| commit.commit),
        "uncommitted_paths": leftover,
        "push": pushed,
        "pr_created": false,
        "task_status": status.to_string(),
        "task_spec_digest": recorded_spec_digest(host, &task.id)?,
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

/// Whether the in-progress rebase is the one `sync_base` started from this
/// run's `prepare_branch` checkpoint: its `orig-head`, `onto`, and
/// `head-name` must name the prepared head SHA, base SHA, and branch. Without
/// that checkpoint no Orbit step started a rebase, so none is owned.
fn prepared_attempt_owns_rebase(input: &Value, workspace_path: &Path) -> Result<bool, OrbitError> {
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
fn refuse_foreign_rebase<H: RuntimeHost + ?Sized>(
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
fn release_review_attempts<H: RuntimeHost + ?Sized>(host: &H, input: &Value, run_id: &str) {
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
