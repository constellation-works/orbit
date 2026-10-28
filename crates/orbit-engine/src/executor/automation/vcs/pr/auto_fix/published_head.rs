use orbit_common::OrbitError;
use orbit_types::workflow::pr_auto_fix_pushes;
use serde_json::Value;

use crate::context::RuntimeHost;

use super::super::delivery::DeliveryPin;
use super::{admission, ledger};

/// Pin the contiguous heads this execution or its authenticated retry
/// ancestors published for the same pull request. Branch and review pins
/// remain enforced by completion on every provider read.
pub(in crate::executor::automation::vcs::pr) fn adopt_published_repairs<H: RuntimeHost + ?Sized>(
    host: &H,
    auto_fix: &admission::AutoFix,
    pr_number: &str,
    pin: &mut DeliveryPin,
) -> Result<(), OrbitError> {
    let history = host.get_task_history(auto_fix.primary_task()?)?;
    for push in pr_auto_fix_pushes(&history, pr_number) {
        if auto_fix.ledger_runs.contains(&push.run_id)
            && pin.candidate_sha() == Some(&push.from_head_sha)
        {
            pin.adopt_refreshed_candidate(Some(&push.to_head_sha));
        }
    }
    Ok(())
}

/// Resolve a completion recovery's original publication through the same
/// contiguous, host-recorded push chain the watcher adopts. Provider input
/// and the live branch cannot supply a replacement head or retry authority.
pub(crate) fn recovery_published_head(
    host: &dyn RuntimeHost,
    run_id: &str,
    task_id: &str,
    pr_number: &str,
    published_head: &str,
) -> Result<String, OrbitError> {
    let ledger_runs = ledger::repair_ledger_runs(host, run_id)?;
    let history = host.get_task_history(task_id)?;
    let mut head = published_head.to_string();
    for push in pr_auto_fix_pushes(&history, pr_number) {
        if ledger_runs.contains(&push.run_id) && head == push.from_head_sha {
            head = push.to_head_sha;
        }
    }
    Ok(head)
}

/// Nested `failed_step_input` field the conflict-recovery provider compares
/// to `git rev-parse ORIG_HEAD`.
///
/// Admission recomputes [`recovery_published_head`] and must not read this
/// field. A rendered value can only make the provider refuse.
pub(crate) const AUTHENTICATED_PUBLISHED_HEAD_SHA: &str = "authenticated_published_head_sha";

/// The publication pin a conflict-recovery provider may trust.
///
/// `pr_complete` walks the same contiguous, host-recorded push chain as
/// admission, starting from the raw `head_sha` / `published_head_sha`. Every
/// other recovery pins that raw head, which for `sync_base` is `head_sha`.
/// `None` withholds the pin when the primary task or pull request cannot be
/// resolved the way admission resolves them, rather than guessing. The raw
/// publication fields are not modified.
pub(crate) fn provider_authenticated_published_head(
    host: &dyn RuntimeHost,
    run_id: &str,
    recovery_input: &Value,
    failed_step_input: &Value,
) -> Result<Option<String>, OrbitError> {
    let Some(published) = failed_step_input
        .get("head_sha")
        .or_else(|| failed_step_input.get("published_head_sha"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    if recovery_input.get("activity_name").and_then(Value::as_str) != Some("pr_complete") {
        return Ok(Some(published.to_string()));
    }
    let Some(task_id) = completion_recovery_primary_task(recovery_input, failed_step_input) else {
        return Ok(None);
    };
    let Some(pr_number) = failed_step_input
        .get("pr_number")
        .and_then(Value::as_str)
        .filter(|pr_number| !pr_number.is_empty())
    else {
        return Ok(None);
    };
    recovery_published_head(host, run_id, task_id, pr_number, published).map(Some)
}

/// Write [`provider_authenticated_published_head`] into a recovery input that
/// already nests `failed_step_input`. A withheld or failed resolution removes
/// a caller-supplied pin so the provider cannot trust one the host did not
/// authenticate.
pub(crate) fn apply_authenticated_published_head(
    host: &dyn RuntimeHost,
    run_id: &str,
    recovery_input: &mut Value,
) {
    let Some(failed) = recovery_input.get("failed_step_input").cloned() else {
        return;
    };
    let pin = provider_authenticated_published_head(host, run_id, recovery_input, &failed)
        .ok()
        .flatten();
    let Some(object) = recovery_input
        .get_mut("failed_step_input")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    match pin {
        Some(head) => {
            object.insert(
                AUTHENTICATED_PUBLISHED_HEAD_SHA.to_string(),
                Value::String(head),
            );
        }
        None => {
            object.remove(AUTHENTICATED_PUBLISHED_HEAD_SHA);
        }
    }
}

/// Admission's primary task for a completion recovery: the minimum
/// `completed_task_ids`, else the minimum recovery `task_ids`, else the
/// recovery input's own task (the worktree guard's task when that input
/// carries `task_id`).
fn completion_recovery_primary_task<'a>(
    recovery_input: &'a Value,
    failed_step_input: &'a Value,
) -> Option<&'a str> {
    failed_step_input
        .get("completed_task_ids")
        .or_else(|| recovery_input.get("task_ids"))
        .and_then(Value::as_array)
        .and_then(|ids| ids.iter().filter_map(Value::as_str).min())
        .or_else(|| recovery_input_task(recovery_input))
}

/// The task a worktree guard records from this recovery input.
///
/// `task_id` wins, matching `WorktreeBoundaryGuard`'s capture. `"unknown"`
/// is that guard's empty sentinel and falls through to a single `task_ids`
/// entry, then back to the sentinel itself.
fn recovery_input_task(input: &Value) -> Option<&str> {
    let direct = input.get("task_id").and_then(Value::as_str);
    if let Some(id) = direct.filter(|id| *id != "unknown") {
        return Some(id);
    }
    match input.get("task_ids").and_then(Value::as_array) {
        Some(ids) if ids.len() == 1 => ids[0].as_str(),
        Some(_) => None,
        None => direct,
    }
}
