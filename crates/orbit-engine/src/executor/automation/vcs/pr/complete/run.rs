//! Completing a pull request delivery: input pins, completion and task updates.

use orbit_common::OrbitError;
use serde_json::{Value, json};

use crate::context::{ReviewLandingRequest, RuntimeHost};

use super::super::super::super::input::input_string_field;
use super::super::super::super::task_update::{authorization_note, complete_tasks};
use super::super::super::handoff::load_handoff_context;
use super::super::auto_fix::AutoFix;
use super::drive::{Delivery, drive_pr_to_merged};
use super::mutation::{ensure_all_tasks_no_diff_expected, resolve_pr_number};
use super::reviewed::reviewed_head_sha;

pub(in crate::executor::automation) fn pr_complete<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<Value, OrbitError> {
    let pinned = landing_review_pins(input);
    let input = pinned.as_ref().unwrap_or(input);
    // Resolve the bundle exactly as `pr_promote` does: the shared handoff
    // context validates that every named task still belongs to this run and has
    // not been diverted, which is the same precondition completion needs.
    let context = load_handoff_context(host, input, "pr_complete")?;

    // A `no-diff-expected` bundle delivered nothing to merge, so there is no PR
    // to verify. Its validation *is* the delivery, and completion authority
    // covers it. Untagged already-landed or verified no-diff work must instead
    // recheck the exact accepted evidence, mirroring the guard `pr_promote`
    // applies.
    let no_diff_expected = input
        .get("no_diff_expected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let task_ids = context
        .tasks
        .iter()
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    let mut delivery_fragment = None;
    let merge_outcome = if no_diff_expected {
        if let Some(checkpoint) = super::super::super::commit::verified_clean_tree_checkpoint(input)
        {
            super::super::super::commit::verify_clean_tree_handoff(
                host,
                &context.tasks,
                &context.workspace_path,
                input_string_field(input, "run_id")
                    .as_deref()
                    .unwrap_or(&context.batch_id),
                checkpoint,
            )?;
            // Name the covering commit in the done transition, so the landing
            // is readable from task history (and from the comment that closes
            // the task's stale PRs).
            delivery_fragment = checkpoint
                .pointer("/already_landed/covering_commit")
                .and_then(Value::as_str)
                .map(|commit| format!("already landed as {commit}"));
            json!({ "merged": false, "reason": checkpoint["decision"], "evidence": checkpoint })
        } else {
            ensure_all_tasks_no_diff_expected(&context.tasks)?;
            json!({ "merged": false, "reason": "no_diff_expected" })
        }
    } else {
        let workspace_path = context.workspace_path.to_string_lossy().into_owned();
        let pr_number = resolve_pr_number(input, &context.tasks)?;
        let auto_fix = AutoFix::from_input(host, input, &task_ids, &context.batch_id)?;
        let delivered = match drive_pr_to_merged(
            host,
            input,
            &workspace_path,
            &pr_number,
            auto_fix.as_ref(),
        )? {
            Delivery::Merged(delivered) => delivered,
            Delivery::RebasedForReview { rebased, reason } => {
                return Ok(json!({
                    "phase": "complete",
                    "no_diff_expected": false,
                    "re_review_required": true,
                    "rebased": rebased,
                    "merge": {
                        "merged": false,
                        "reason": reason,
                        "pr_number": pr_number,
                        "reviewed_head_sha": reviewed_head_sha(input),
                    },
                    "completed_task_ids": [],
                    "skipped_task_ids": [],
                }));
            }
            // Repair authority alone: the watch ends here and the task
            // stays in review.
            Delivery::Watched(outcome) => {
                return Ok(json!({
                    "phase": "complete",
                    "no_diff_expected": false,
                    "re_review_required": false,
                    "merge": outcome,
                    "completed_task_ids": [],
                    "skipped_task_ids": [],
                }));
            }
        };
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
                managed_merge: delivered.outcome["managed_merge"]
                    .as_bool()
                    .unwrap_or(false),
                landed_commit: delivered
                    .outcome
                    .get("landed_commit")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })?;
        }
        delivery_fragment = Some(delivered.evidence.authorization_fragment());
        delivered.outcome
    };

    // [ORB-11982] Name the evidence that permitted `done` in durable task
    // history, so a later reader can tell this transition apart from one an
    // unattributed writer applied.
    let authorization = match delivery_fragment {
        Some(fragment) => format!(
            "{}; {fragment}",
            authorization_note(input, &context.batch_id)
        ),
        None => authorization_note(input, &context.batch_id),
    };
    let completion = complete_tasks(host, &context.batch_id, &task_ids, &authorization)?;

    Ok(json!({
        "phase": "complete",
        "no_diff_expected": no_diff_expected,
        "re_review_required": false,
        "merge": merge_outcome,
        "completed_task_ids": completion["completed_task_ids"],
        "skipped_task_ids": completion["skipped_task_ids"],
        "authorization": completion["authorization"],
    }))
}

/// The completion pins after a before-landing review settled
/// `landing_reviewed_head_sha` [ORB-14849], or `None` when no such review
/// ran. That head is the one to merge. A reviewer fix was pushed onto the
/// published head under a lease, so a different settled head is now the
/// published one and the head it replaced the previous one.
fn landing_review_pins(input: &Value) -> Option<Value> {
    let landing = input_string_field(input, "landing_reviewed_head_sha")?;
    let mut pinned = input.clone();
    if input_string_field(input, "published_head_sha").as_deref() != Some(landing.as_str()) {
        pinned["previous_published_head_sha"] = input
            .get("published_head_sha")
            .cloned()
            .unwrap_or(Value::Null);
        pinned["published_head_sha"] = json!(landing);
    }
    pinned["reviewed_head_sha"] = json!(landing);
    Some(pinned)
}
