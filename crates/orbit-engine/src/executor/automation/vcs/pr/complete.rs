//! Completion-authorized PR delivery [ORB-11187].
//!
//! `pr_promote` ends a PR-mode run at `review`. When the operator granted this
//! invocation completion authority (`--complete`), this step carries the same
//! PR the rest of the pipeline opened the rest of the way: it drives the merge
//! through GitHub's own gates, verifies the merge actually happened, and only
//! then runs the guarded `review -> done` transition.
//!
//! The invariant this module exists to hold is that *merged* is established by
//! reading GitHub's merged state back, never inferred from having asked. In
//! particular, enabling auto-merge is not terminal success: the poll continues
//! until the PR reports `MERGED`, or the wait budget expires and the run fails
//! with the tasks still in `review`.
//!
//! Branch protection is respected by construction. The merge request is an
//! ordinary provider merge (ungated runs may use `--auto`); no administrative
//! bypass is reachable, so a PR that GitHub reports as `BLOCKED` fails the
//! run rather than being forced through.
//!
//! A `DIRTY` PR is narrower than those policy refusals. With the pipeline's
//! retained `completion: done`, published-head, branch, and base checkpoints,
//! completion reuses the ordinary pinned `git_rebase` and lease-checked push
//! boundaries. Only a rebase that proves unmerged index entries can reach the
//! existing bounded conflict-recovery leaf.

use std::thread::sleep;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use serde_json::{Value, json};

use crate::context::{ReviewLandingRequest, RuntimeHost};

use super::super::super::ci::bounded_u64;
use super::super::super::input::input_string_field;
use super::super::super::task_update::{authorization_note, complete_tasks};
use super::super::freshness::{
    branch_freshness_against_ref, commit_sha, rebase_pr_branch, remote_branch_sha,
};
use super::super::git::{base_sync_mode_from_input, resolve_worktree_start_point};
use super::super::handoff::load_handoff_context;
use super::super::operations;
use super::super::push::push_batch_changes;
use super::merge::{MergeCapabilities, MergeStrategy, resolve_merge_capabilities};

/// Default budget for waiting out required checks before giving up.
const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;
const MAX_WAIT_SECONDS: u64 = 6 * 60 * 60;
const MIN_POLL_INTERVAL_SECONDS: u64 = 5;
const MAX_POLL_INTERVAL_SECONDS: u64 = 10 * 60;

pub(in crate::executor::automation) fn pr_complete<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    // Resolve the bundle exactly as `pr_promote` does: the shared handoff
    // context validates that every named task still belongs to this run and has
    // not been diverted, which is the same precondition completion needs.
    let context = load_handoff_context(host, input, "pr_complete")?;

    // A `no-diff-expected` bundle delivered nothing to merge, so there is no PR
    // to verify. Its validation *is* the delivery, and completion authority
    // covers it. Untagged already-landed work must instead recheck the exact
    // accepted evidence, mirroring the same guard `pr_promote` applies.
    let no_diff_expected = input
        .get("no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let task_ids = context
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let merge_outcome = if no_diff_expected {
        if let Some(checkpoint) = input.get("already_landed_checkpoint").filter(|value| {
            value.get("decision").and_then(Value::as_str) == Some("verified_already_landed")
        }) {
            super::super::commit::already_landed::verify_handoff(
                host,
                &context.tasks,
                &context.workspace_path,
                input_string_field(input, "run_id")
                    .as_deref()
                    .unwrap_or(&context.batch_id),
                checkpoint,
            )?;
            json!({ "merged": false, "reason": "verified_already_landed", "evidence": checkpoint })
        } else {
            ensure_all_tasks_no_diff_expected(&context.tasks)?;
            json!({ "merged": false, "reason": "no_diff_expected" })
        }
    } else {
        let workspace_path = context.workspace_path.to_string_lossy().into_owned();
        let pr_number = resolve_pr_number(input, &context.tasks)?;
        let outcome = drive_pr_to_merged(host, input, &workspace_path, &pr_number)?;
        // [ORB-11333] The certificate only carries into coverage when the
        // landing that actually happened is verified against it.
        if let Some(reviewed_head_sha) = reviewed_head_sha(input) {
            host.record_review_landing(&ReviewLandingRequest {
                run_id: context.batch_id.clone(),
                task_ids: task_ids.clone(),
                workspace_path: context.workspace_path.clone(),
                pr_number: pr_number.clone(),
                base: input_string_field(input, "base").unwrap_or_default(),
                reviewed_head_sha,
                managed_merge: outcome["managed_merge"].as_bool().unwrap_or(false),
                landed_commit: outcome
                    .get("landed_commit")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })?;
        }
        outcome
    };
    let authorization = authorization_note(input, &context.batch_id);
    let completion = complete_tasks(host, &context.batch_id, &task_ids, &authorization)?;

    Ok(json!({
        "phase": "complete",
        "no_diff_expected": no_diff_expected,
        "merge": merge_outcome,
        "completed_task_ids": completion["completed_task_ids"],
        "skipped_task_ids": completion["skipped_task_ids"],
        "authorization": completion["authorization"],
    }))
}

/// Poll GitHub until the PR is verifiably merged, requesting the merge (or
/// auto-merge) as its reported state allows.
fn drive_pr_to_merged<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
) -> Result<Value, OrbitError> {
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
    let mut waited_seconds = 0_u64;
    let mut auto_merge_requested = false;
    let mut merge_requested = false;
    let mut requested_landed_commit: Option<String> = None;
    let mut conflict_refresh_attempted = false;
    let mut merge_capabilities: Option<MergeCapabilities> = None;

    let reviewed_head_sha = reviewed_head_sha(input);

    loop {
        let status = read_pr_status(host, workspace_path, pr_number)?;
        match classify(&status) {
            PrMergeState::Merged => {
                let landed_commit = status.pointer("/mergeCommit/oid").and_then(Value::as_str);
                let managed_merge = requested_landed_commit
                    .as_deref()
                    .is_some_and(|requested| Some(requested) == landed_commit);
                return Ok(json!({
                    "merged": true,
                    "pr_number": pr_number,
                    "strategy": merge_capabilities.map(|capabilities| capabilities.strategy.as_str()),
                    "auto_merge_requested": auto_merge_requested,
                    "waited_seconds": waited_seconds,
                    "max_wait_seconds": max_wait_seconds,
                    "poll_interval_seconds": poll_interval_seconds,
                    "landed_commit": landed_commit,
                    "managed_merge": managed_merge,
                    "reviewed_head_sha": reviewed_head_sha,
                }));
            }
            PrMergeState::Closed => {
                return Err(OrbitError::Execution(format!(
                    "pr_complete: pull request #{pr_number} was closed without being merged; \
                     the task stays in review"
                )));
            }
            PrMergeState::Blocked(reason) => {
                return Err(OrbitError::Execution(format!(
                    "pr_complete: pull request #{pr_number} cannot be merged ({reason}); \
                     branch protection or required reviews are unsatisfied and this run does not \
                     bypass them — the task stays in review"
                )));
            }
            PrMergeState::Conflict => {
                // [ORB-11333] Repairing a conflict rewrites the candidate; a
                // reviewed head cannot be merged as unreviewed content.
                if reviewed_head_sha.is_some() {
                    return Err(OrbitError::Execution(format!(
                        "review_gate_stale: pull request #{pr_number} has merge conflicts and its \
                         head is bound to a before-PR review; a conflict repair needs a fresh \
                         review before managed completion, so the task stays in review"
                    )));
                }
                if conflict_refresh_attempted || !has_completion_recovery_checkpoint(input) {
                    return Err(completion_conflict_error(pr_number));
                }
                refresh_conflicting_pr_branch(host, input, workspace_path, pr_number, &status)?;
                conflict_refresh_attempted = true;
                // The existing PR remains authoritative. Re-read its state
                // after the branch update rather than inferring mergeability
                // from a successful push.
                continue;
            }
            PrMergeState::Mergeable => {
                ensure_pr_head_is_reviewed(&status, pr_number, reviewed_head_sha.as_deref())?;
                if !merge_requested {
                    let capabilities = resolved_capabilities(
                        host,
                        workspace_path,
                        pr_number,
                        &mut merge_capabilities,
                    )?;
                    requested_landed_commit = request_merge(
                        host,
                        workspace_path,
                        pr_number,
                        capabilities.strategy,
                        false,
                        reviewed_head_sha.as_deref(),
                    )
                    .map_err(|error| {
                        OrbitError::Execution(format!(
                            "pr_complete: could not request {} merge on pull request \
                                 #{pr_number}: {error}; the task stays in review",
                            capabilities.strategy.as_str()
                        ))
                    })?;
                    merge_requested = true;
                    // Re-read rather than assuming the request landed.
                    continue;
                }
            }
            PrMergeState::Pending => {
                ensure_pr_head_is_reviewed(&status, pr_number, reviewed_head_sha.as_deref())?;
                // The CLI auto-merge path cannot retain the review condition.
                // Gated runs wait locally and use the conditional synchronous
                // mutation once checks settle, including on queue-only branches
                // where that mutation will refuse the unsupported merge.
                if reviewed_head_sha.is_none() && !auto_merge_requested {
                    // Required checks are still running. Hand the merge to
                    // GitHub's auto-merge when this repository allows it, then
                    // keep polling: enabling it is not success. Repositories
                    // with auto-merge disabled instead wait for a normally
                    // mergeable state, at which point the ordinary merge path
                    // above makes the same permitted request.
                    let capabilities = resolved_capabilities(
                        host,
                        workspace_path,
                        pr_number,
                        &mut merge_capabilities,
                    )?;
                    if capabilities.auto_merge_allowed {
                        request_merge(
                            host,
                            workspace_path,
                            pr_number,
                            capabilities.strategy,
                            true,
                            None,
                        )
                        .map_err(|error| {
                            OrbitError::Execution(format!(
                                "pr_complete: could not enable auto-merge using {} on pull request \
                                     #{pr_number}: {error}; the task stays in review",
                                capabilities.strategy.as_str()
                            ))
                        })?;
                        auto_merge_requested = true;
                    }
                }
            }
        }

        if waited_seconds >= max_wait_seconds {
            return Err(OrbitError::Execution(format!(
                "pr_complete: timed out after {waited_seconds}s waiting for pull request \
                 #{pr_number} to merge (budget {max_wait_seconds}s); the task stays in review"
            )));
        }
        let remaining_seconds = max_wait_seconds.saturating_sub(waited_seconds);
        let sleep_seconds = poll_interval_seconds.min(remaining_seconds);
        sleep(Duration::from_secs(sleep_seconds));
        waited_seconds = waited_seconds.saturating_add(sleep_seconds);

        if waited_seconds >= max_wait_seconds {
            return Err(OrbitError::Execution(format!(
                "pr_complete: timed out after {waited_seconds}s waiting for pull request \
                 #{pr_number} to merge (budget {max_wait_seconds}s); the task stays in review"
            )));
        }
    }
}

/// What GitHub's reported PR state means for a completion attempt.
enum PrMergeState {
    Merged,
    Closed,
    /// Merging is refused by a gate this run must not bypass.
    Blocked(String),
    /// GitHub reports a content conflict. The local rebase boundary must prove
    /// actual unmerged entries before an agent may be launched.
    Conflict,
    /// Ready to merge now.
    Mergeable,
    /// Required checks are still in flight.
    Pending,
}

fn classify(pull_request: &Value) -> PrMergeState {
    let state = pull_request
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_uppercase();
    let merged_at = pull_request.get("mergedAt").and_then(Value::as_str);
    if state == "MERGED" || merged_at.is_some_and(|value| !value.trim().is_empty()) {
        return PrMergeState::Merged;
    }
    if state == "CLOSED" {
        return PrMergeState::Closed;
    }

    let merge_state = pull_request
        .get("mergeStateStatus")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_uppercase();
    match merge_state.as_str() {
        // Mergeable now: no gate outstanding, or only non-required signals.
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE" => PrMergeState::Mergeable,
        // Required checks still running.
        "PENDING" => PrMergeState::Pending,
        // Requires human action this run is not authorized to substitute for.
        "BLOCKED" => PrMergeState::Blocked("required reviews or checks are not satisfied".into()),
        "DIRTY" => PrMergeState::Conflict,
        "BEHIND" => {
            PrMergeState::Blocked("the branch is behind its base and must be updated".into())
        }
        "DRAFT" => PrMergeState::Blocked("the pull request is still a draft".into()),
        // An empty or unrecognized merge state is treated as still settling:
        // GitHub reports UNKNOWN while it computes mergeability.
        _ => PrMergeState::Pending,
    }
}

/// The head a before-PR gate settled, when this run carried one.
fn reviewed_head_sha(input: &Value) -> Option<String> {
    input_string_field(input, "reviewed_head_sha")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Early diagnostic for an already moved head. The provider mutation must
/// also enforce the reviewed SHA atomically; this read cannot prevent a race.
fn ensure_pr_head_is_reviewed(
    status: &Value,
    pr_number: &str,
    reviewed_head_sha: Option<&str>,
) -> Result<(), OrbitError> {
    let Some(reviewed) = reviewed_head_sha else {
        return Ok(());
    };
    let reported = status
        .get("headRefOid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "review_gate_stale: pull request #{pr_number} did not report its head commit, so \
                 the reviewed candidate {reviewed} cannot be pinned before merging; the task stays \
                 in review"
            ))
        })?;
    if reported != reviewed {
        return Err(OrbitError::Execution(format!(
            "review_gate_stale: pull request #{pr_number} head {reported} is not the reviewed \
             candidate {reviewed}; later revisions need a fresh review before managed \
             completion, so the task stays in review"
        )));
    }
    Ok(())
}

fn has_completion_recovery_checkpoint(input: &Value) -> bool {
    input.get("completion").and_then(Value::as_str) == Some("done")
        && ["head", "published_head_sha", "base"]
            .into_iter()
            .all(|key| input_string_field(input, key).is_some())
}

fn completion_conflict_error(pr_number: &str) -> OrbitError {
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
fn refresh_conflicting_pr_branch<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
    workspace_path: &str,
    pr_number: &str,
    pull_request: &Value,
) -> Result<(), OrbitError> {
    let head =
        input_string_field(input, "head").ok_or_else(|| completion_conflict_error(pr_number))?;
    let published_head_sha = input_string_field(input, "published_head_sha")
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let base =
        input_string_field(input, "base").ok_or_else(|| completion_conflict_error(pr_number))?;
    let workspace = std::path::Path::new(workspace_path);

    let pr_head = input_string_field(pull_request, "headRefName")
        .ok_or_else(|| completion_conflict_error(pr_number))?;
    let pr_base = input_string_field(pull_request, "baseRefName")
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
    let target_base_sha = commit_sha(workspace, &base_ref)?;
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
    let synced = rebase_pr_branch(host, &rebase_input)?;
    let push_input = json!({
        "job_run_id": input.get("job_run_id").cloned().unwrap_or(Value::Null),
        "completed_task_ids": input.get("completed_task_ids").cloned().unwrap_or(Value::Null),
        "workspace_path": workspace_path,
        "branch": synced["head"],
        "rewrite_performed": synced["rewritten"],
        "rewrite_head_before": synced["head_sha_before"],
        "expected_remote_sha": synced["remote_sha_before"],
    });
    push_batch_changes(host, &push_input)?;
    Ok(())
}

fn read_pr_status<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
) -> Result<Value, OrbitError> {
    let response = host.run_private_vcs_operation(
        operations::PR_STATUS,
        json!({
            "pr": pr_number,
            "workspace_path": workspace_path,
        }),
    )?;
    Ok(response.get("pull_request").cloned().unwrap_or(Value::Null))
}

fn request_merge<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
    strategy: MergeStrategy,
    auto: bool,
    reviewed_head_sha: Option<&str>,
) -> Result<Option<String>, OrbitError> {
    host.run_private_vcs_operation(
        operations::PR_MERGE,
        json!({
            "pr": pr_number,
            "strategy": strategy.as_str(),
            "auto": auto,
            "reviewed_head_sha": reviewed_head_sha,
            "workspace_path": workspace_path,
        }),
    )
    .map(|result| {
        result
            .get("landed_commit")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    })
}

fn resolved_capabilities<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace_path: &str,
    pr_number: &str,
    selected: &mut Option<MergeCapabilities>,
) -> Result<MergeCapabilities, OrbitError> {
    if let Some(capabilities) = *selected {
        return Ok(capabilities);
    }
    let capabilities =
        resolve_merge_capabilities(host, workspace_path, pr_number).map_err(|error| {
            OrbitError::Execution(format!(
                "pr_complete: could not resolve a permitted merge method for pull request \
             #{pr_number}: {error}; the task stays in review"
            ))
        })?;
    *selected = Some(capabilities);
    Ok(capabilities)
}

fn resolve_pr_number(input: &Value, tasks: &[Task]) -> Result<String, OrbitError> {
    if let Some(pr_number) = input_string_field(input, "pr_number") {
        return Ok(pr_number);
    }
    tasks
        .iter()
        .find_map(Task::github_pr_number)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "pr_complete: no pr_number supplied and no task in the batch carries a github-pr \
                 external ref"
                    .to_string(),
            )
        })
}

fn ensure_all_tasks_no_diff_expected(tasks: &[Task]) -> Result<(), OrbitError> {
    if tasks
        .iter()
        .any(|task| !task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG))
    {
        return Err(OrbitError::Execution(
            "pr_complete: no_diff_expected requires every task to carry the no-diff-expected tag"
                .to_string(),
        ));
    }
    Ok(())
}
