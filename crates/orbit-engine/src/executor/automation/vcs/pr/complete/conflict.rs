//! Admitting, refreshing and rebasing a pull request branch that conflicts with its base.

use orbit_common::OrbitError;
use orbit_types::workflow::PrAutoFixBlocker;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::super::input::input_string_field;
use super::super::super::freshness::{
    branch_freshness_against_ref, commit_sha, rebase_pr_branch, remote_branch_sha,
};
use super::super::super::git::{base_sync_mode_from_input, resolve_worktree_start_point};
use super::super::super::push::push_batch_changes;
use super::super::answers::PullRequestStatus;
use super::super::auto_fix::{AutoFix, admit, stopped};
use super::mutation::record_completion_mutation;

pub(super) fn re_review_on_conflict(input: &Value) -> bool {
    input
        .get("re_review_on_conflict")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Whether the PR reports `BLOCKED` itself, rather than another state
/// completion also refuses (`BEHIND`, `DRAFT`, or an explicit draft), which
/// auto-fix never repairs.
pub(super) fn is_blocked_merge_state(status: &PullRequestStatus) -> bool {
    !status.is_draft
        && status
            .merge_state_status
            .as_deref()
            .is_some_and(|state| state.eq_ignore_ascii_case("BLOCKED"))
}

/// Admit an auto-fix repair of the conflict on the pull request's head
/// before the pinned rebase can reach conflict recovery.
pub(super) fn admit_conflict_repair<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &AutoFix,
    pr_number: &str,
    status: &PullRequestStatus,
) -> Result<(), OrbitError> {
    let head_sha = status
        .head_sha
        .as_deref()
        .ok_or_else(|| stopped(pr_number, "the pull request reported no head commit"))?;
    admit(
        host,
        auto_fix,
        pr_number,
        head_sha,
        PrAutoFixBlocker::Conflict,
    )
    .map(|_| ())
}

/// Completion, or auto-fix repair, authority over the retained delivery
/// checkpoints a conflict repair needs.
pub(super) fn has_completion_recovery_checkpoint(input: &Value) -> bool {
    (input.get("completion").and_then(Value::as_str) == Some("done")
        || input.get("auto_fix").and_then(Value::as_bool) == Some(true))
        && ["head", "published_head_sha", "base"]
            .into_iter()
            .all(|key| input_string_field(input, key).is_some())
}

pub(super) fn completion_conflict_error(pr_number: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "pr_complete: pull request #{pr_number} cannot be merged (the branch has merge conflicts); \
         the task stays in review"
    ))
}

/// Reconcile a published candidate with the base that GitHub currently sees.
///
/// This deliberately composes the existing pinned rebase and lease-checked
/// push boundaries. A real conflict therefore becomes the same typed
/// `RecoverableVcsConflict` as the pre-publication synchronization step; all
/// other fetch, ownership, authorization, and push failures stay untyped and
/// cannot launch the conflict-recovery agent.
///
/// Returns the rewritten candidate SHA now published on the branch, which
/// completion adopts for subsequent provider readback and delivery checks.
pub(super) fn refresh_conflicting_pr_branch<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    pull_request: &PullRequestStatus,
) -> Result<Option<String>, OrbitError> {
    let synced =
        rebase_conflicting_pr_branch(host, input, workspace_path, pr_number, pull_request)?;
    let push_input = json!({
        "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
        "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
        "workspace_path": workspace_path,
        "branch": synced["head"],
        "rewrite_performed": synced["rewritten"],
        "rewrite_head_before": synced["head_sha_before"],
        "expected_remote_sha": synced["remote_sha_before"],
        "lease_remote_sha": synced["remote_sha_before"],
    });
    push_batch_changes(host, &push_input)?;
    Ok(synced
        .get("head_sha")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

/// Rebase the published candidate onto the base GitHub currently sees,
/// locally, returning the `git_rebase` checkpoint a lease-checked push of the
/// rewritten branch needs.
pub(super) fn rebase_conflicting_pr_branch<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    pull_request: &PullRequestStatus,
) -> Result<Value, OrbitError> {
    let head =
        input_string_field(input, "head").ok_or_else(|| completion_conflict_error(pr_number))?;
    let published_head_sha = input_string_field(input, "published_head_sha")
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let base =
        input_string_field(input, "base").ok_or_else(|| completion_conflict_error(pr_number))?;
    let workspace = std::path::Path::new(workspace_path);

    let pr_head = pull_request
        .head_branch
        .clone()
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let pr_base = pull_request
        .base_branch
        .clone()
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let expected_base = base.strip_prefix("origin/").unwrap_or(&base);
    if pr_head != head || pr_base != expected_base {
        return Err(OrbitError::Execution(format!(
            "pr_complete: pull request #{pr_number} identity changed from branch '{head}' into \
             '{expected_base}' to branch '{pr_head}' into '{pr_base}'; refusing completion \
             conflict repair"
        )));
    }

    let observed_remote_sha = remote_branch_sha(workspace, &head)?;
    if observed_remote_sha.as_deref() != Some(published_head_sha.as_str()) {
        return Err(OrbitError::Execution(format!(
            "pr_complete: published branch 'origin/{head}' moved away from completion checkpoint \
             '{published_head_sha}'; refusing to replace stale or concurrently updated candidate \
             while pull request #{pr_number} stays in review"
        )));
    }

    let base_ref =
        resolve_worktree_start_point(workspace, &base, base_sync_mode_from_input(input)?)?;
    let observed_base_sha = commit_sha(workspace, &base_ref)?;
    if let Some(pinned) = input_string_field(input, "landed_repair_base_sha")
        && pinned != observed_base_sha
    {
        return Err(stopped(
            pr_number,
            "base advanced after the landed repair was diagnosed",
        ));
    }
    let target_base_sha = observed_base_sha;
    let published_freshness =
        branch_freshness_against_ref(workspace, &published_head_sha, &base_ref, &target_base_sha)?;
    if published_freshness.commits_ahead == 0 {
        return Err(OrbitError::Execution(format!(
            "pr_complete: published candidate '{published_head_sha}' is no longer ahead of \
             target base checkpoint '{target_base_sha}'; refusing completion conflict repair"
        )));
    }
    if published_freshness.commits_behind == 0 {
        return Err(completion_conflict_error(pr_number));
    }

    let rebase_input = json!({
        "run_id": input.get("run_id").cloned().unwrap_or(Value::Null),
        "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
        "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
        "workspace_path": workspace_path,
        "head": head,
        "head_sha": published_head_sha,
        "base": base,
        "base_ref": base_ref,
        "base_sha": target_base_sha,
        "remote_sha": observed_remote_sha,
        "commits_behind": published_freshness.commits_behind,
        "sync_required": true,
    });
    record_completion_mutation(host, input, "rebase")?;
    let rebased = rebase_pr_branch(host, &rebase_input)?;
    // Auto-fix can refresh inside the watcher without returning through the
    // pipeline's validation step. Validate the exact rewrite before either
    // publication or the independent re-review handoff.
    if input.get("auto_fix").and_then(Value::as_bool) == Some(true) {
        super::super::super::candidate_validate::candidate_validate(
            host,
            &json!({
                "job_run_id":input["job_run_id"], "completed_task_ids":input["completed_task_ids"],
                "workspace_path":workspace_path, "base_sha":rebased["base_sha"], "base_ref":rebased["base_ref"],
            }),
        )?;
    }
    Ok(rebased)
}
