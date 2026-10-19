//! Light delivery [ORB-16855]: whether a candidate keeps its tasks'
//! `skip-review` or `skip-validation` tag.
//!
//! The allow-list is the host's `workflow.light_delivery_paths`. A claimed
//! leaf reads the owner's captured list from its contract, so the leaf and
//! its owner judge one candidate alike; an owner too old to capture one, or a
//! replica host, has no list, so no candidate is light and both tags are
//! ignored. Each decision is recorded once per candidate in every delivered
//! task's history; a claimed leaf cannot write history, so its owner records
//! the decision when it accepts the handoff.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::path_filter::PathFilter;
use orbit_types::task::{
    LIGHT_DELIVERY_PATHS_KEY, LightDeliveryDecision, LightDeliveryJudgement, TaskHistoryEntry,
};

use crate::context::{RuntimeHost, TaskAutomationUpdate};

use super::git::git_output_paths;

/// Judge the paths `head` changes against `base` in `workspace` (deletions
/// included, renames as a deletion and an addition) against `allow`.
///
/// # Errors
/// Returns an error when `allow` holds an invalid glob or Git cannot diff the
/// two commits.
pub fn judge_light_delivery(
    workspace: &Path,
    base: &str,
    head: &str,
    allow: &[String],
) -> Result<LightDeliveryJudgement, OrbitError> {
    let filter = PathFilter::new(LIGHT_DELIVERY_PATHS_KEY, allow)?;
    let changed = git_output_paths(
        workspace,
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            "--end-of-options",
            base,
            head,
            "--",
        ],
    )?;
    Ok(LightDeliveryJudgement::new(
        changed.len(),
        &filter.outside(&changed),
    ))
}

/// The decision for `tag` over `task_ids` and the candidate `head` on
/// `base`, or `None` when no delivered task carries the tag.
pub(super) fn light_delivery_decision<H: RuntimeHost + ?Sized>(
    host: &H,
    workspace: &Path,
    base: &str,
    head: &str,
    task_ids: &[String],
    tag: &'static str,
) -> Result<Option<LightDeliveryDecision>, OrbitError> {
    let tasks = task_ids
        .iter()
        .map(|task_id| host.get_task(task_id).map(|task| (task_id, task.tags)))
        .collect::<Result<Vec<_>, _>>()?;
    let tagged = tasks
        .iter()
        .map(|(task_id, tags)| (task_id.as_str(), tags.as_slice()))
        .collect::<Vec<_>>();
    if !tagged
        .iter()
        .any(|(_, tags)| tags.iter().any(|carried| carried == tag))
    {
        return Ok(None);
    }
    let judgement = judge_light_delivery(
        workspace,
        base,
        head,
        host.validation_gates().light_delivery_paths(),
    )?;
    Ok(LightDeliveryDecision::decide(tag, &tagged, head, judgement))
}

/// Record `decision` in each delivered task's history unless the task's
/// latest entry already records it, so a retried step records it once.
pub(super) fn record_light_delivery<H: RuntimeHost + ?Sized>(
    host: &H,
    task_ids: &[String],
    decision: &LightDeliveryDecision,
) -> Result<(), OrbitError> {
    if host.worker_invocation().is_some() {
        return Ok(());
    }
    let event = decision.event();
    let note = decision.note();
    for task_id in task_ids {
        let recorded = host
            .get_task_history(task_id)?
            .iter()
            .rev()
            .any(|entry| entry.event == event && entry.note.as_deref() == Some(note.as_str()));
        if recorded {
            continue;
        }
        host.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: event.to_string(),
                    note: Some(note.clone()),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}
