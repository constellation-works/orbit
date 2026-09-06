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
//! ordinary `gh pr merge` (optionally `--auto`); no administrative bypass is
//! reachable from this path, so a PR that GitHub reports as `BLOCKED` fails the
//! run rather than being forced through.

use std::thread::sleep;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::input::input_string_field;
use super::super::super::task_update::{authorization_note, complete_tasks};
use super::super::handoff::load_handoff_context;
use super::super::operations;
use super::merge::{MergeCapabilities, MergeStrategy, resolve_merge_capabilities};

/// Default budget for waiting out required checks before giving up.
const DEFAULT_MAX_WAIT_SECONDS: u64 = 3600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 30;

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
    // covers it — but only when every task in the bundle actually carries the
    // tag, mirroring the same guard `pr_promote` applies.
    let no_diff_expected = input
        .get("no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let merge_outcome = if no_diff_expected {
        ensure_all_tasks_no_diff_expected(&context.tasks)?;
        json!({ "merged": false, "reason": "no_diff_expected" })
    } else {
        let workspace_path = context.workspace_path.to_string_lossy().into_owned();
        let pr_number = resolve_pr_number(input, &context.tasks)?;
        drive_pr_to_merged(host, input, &workspace_path, &pr_number)?
    };

    let task_ids = context
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let authorization = authorization_note(input, &context.batch_id);
    let completion = complete_tasks(host, &task_ids, &authorization)?;

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
    let max_wait_seconds = input_u64(input, "max_wait_seconds").unwrap_or(DEFAULT_MAX_WAIT_SECONDS);
    let poll_interval_seconds =
        input_u64(input, "poll_interval_seconds").unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS);
    let mut waited_seconds = 0_u64;
    let mut auto_merge_requested = false;
    let mut merge_requested = false;
    let mut merge_capabilities: Option<MergeCapabilities> = None;

    loop {
        let status = read_pr_status(host, workspace_path, pr_number)?;
        match classify(&status) {
            PrMergeState::Merged => {
                return Ok(json!({
                    "merged": true,
                    "pr_number": pr_number,
                    "strategy": merge_capabilities.map(|capabilities| capabilities.strategy.as_str()),
                    "auto_merge_requested": auto_merge_requested,
                    "waited_seconds": waited_seconds,
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
            PrMergeState::Mergeable => {
                if !merge_requested {
                    let capabilities = resolved_capabilities(
                        host,
                        workspace_path,
                        pr_number,
                        &mut merge_capabilities,
                    )?;
                    request_merge(
                        host,
                        workspace_path,
                        pr_number,
                        capabilities.strategy,
                        false,
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
                if !auto_merge_requested {
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
                        request_merge(host, workspace_path, pr_number, capabilities.strategy, true)
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
        if poll_interval_seconds > 0 {
            sleep(Duration::from_secs(poll_interval_seconds));
        }
        waited_seconds = waited_seconds.saturating_add(poll_interval_seconds.max(1));
    }
}

/// What GitHub's reported PR state means for a completion attempt.
enum PrMergeState {
    Merged,
    Closed,
    /// Merging is refused by a gate this run must not bypass.
    Blocked(String),
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
        "DIRTY" => PrMergeState::Blocked("the branch has merge conflicts".into()),
        "BEHIND" => {
            PrMergeState::Blocked("the branch is behind its base and must be updated".into())
        }
        "DRAFT" => PrMergeState::Blocked("the pull request is still a draft".into()),
        // An empty or unrecognized merge state is treated as still settling:
        // GitHub reports UNKNOWN while it computes mergeability.
        _ => PrMergeState::Pending,
    }
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
) -> Result<(), OrbitError> {
    host.run_private_vcs_operation(
        operations::PR_MERGE,
        json!({
            "pr": pr_number,
            "strategy": strategy.as_str(),
            "auto": auto,
            "workspace_path": workspace_path,
        }),
    )
    .map(|_| ())
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

fn input_u64(input: &Value, key: &str) -> Option<u64> {
    input.get(key).and_then(Value::as_u64)
}
