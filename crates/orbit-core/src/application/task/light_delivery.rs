//! Light delivery [ORB-16855]: Core's side of the `skip-review` and
//! `skip-validation` tags.
//!
//! The review gate, the owner's handoff acceptance and after-landing
//! coverage each judge a candidate here against this host's
//! `workflow.light_delivery_paths` (a claimed leaf: the owner's captured
//! list), with the same diff the engine's validation steps use, so every
//! layer reaches the same decision for one candidate.

use std::path::Path;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{LIGHT_DELIVERY_TAGS, LightDeliveryDecision, Task, TaskHistoryEntry};
use orbit_types::workflow::handoff::HandoffCandidate;

use crate::OrbitRuntime;
use crate::application::task::TaskJobPort;

/// The decision for `tag` over `tasks` and the candidate `head` on `base`
/// in `workspace`, or `None` when no task carries the tag.
pub(crate) fn light_delivery_decision(
    runtime: &OrbitRuntime,
    workspace: &Path,
    base: &str,
    head: &str,
    tasks: &[Task],
    tag: &'static str,
) -> Result<Option<LightDeliveryDecision>, OrbitError> {
    if !tasks.iter().any(|task| carries(task, tag)) {
        return Ok(None);
    }
    let judgement = orbit_engine::judge_light_delivery(
        workspace,
        base,
        head,
        runtime.candidate_validation_gates().light_delivery_paths(),
    )?;
    let tagged = tasks
        .iter()
        .map(|task| (task.id.as_str(), task.tags.as_slice()))
        .collect::<Vec<_>>();
    Ok(LightDeliveryDecision::decide(tag, &tagged, head, judgement))
}

/// Whether `task` carries `tag`.
pub(crate) fn carries(task: &Task, tag: &str) -> bool {
    task.tags.iter().any(|carried| carried == tag)
}

/// The history entry recording `decision`.
pub(crate) fn light_delivery_history_entry(decision: &LightDeliveryDecision) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: Utc::now(),
        by: "system".into(),
        event: decision.event().to_string(),
        note: Some(decision.note()),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }
}

/// Whether `history` already records `decision`.
pub(crate) fn records_light_delivery(
    history: &[TaskHistoryEntry],
    decision: &LightDeliveryDecision,
) -> bool {
    let note = decision.note();
    history.iter().rev().any(|entry| {
        entry.event == decision.event() && entry.note.as_deref() == Some(note.as_str())
    })
}

/// Record `decision` once in each of `task_ids`' history. A claimed leaf
/// writes no owner history; its owner records the decision at acceptance.
pub(crate) fn record_light_delivery(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    decision: &LightDeliveryDecision,
) -> Result<(), OrbitError> {
    if runtime.worker_invocation().is_some() {
        return Ok(());
    }
    for task_id in task_ids {
        if records_light_delivery(&runtime.get_task_history(task_id)?, decision) {
            continue;
        }
        runtime.apply_task_automation_update(
            task_id,
            TaskAutomationUpdate {
                append_history: vec![light_delivery_history_entry(decision)],
                ..Default::default()
            },
        )?;
    }
    Ok(())
}

impl OrbitRuntime {
    /// This owner's light-delivery decisions for a claimed `task_id`'s
    /// handed-off candidate, judged in its own checkout against its own
    /// allow-list [ORB-16855]. A candidate that changes nothing, or a task
    /// that carries neither tag, has none.
    pub(crate) fn owner_light_delivery(
        &self,
        task_id: &str,
        candidate: &HandoffCandidate,
    ) -> Result<Vec<LightDeliveryDecision>, OrbitError> {
        if candidate.candidate.commit == candidate.base.commit {
            return Ok(Vec::new());
        }
        let tasks = [self.get_task(task_id)?];
        let mut decisions = Vec::new();
        for tag in LIGHT_DELIVERY_TAGS {
            decisions.extend(light_delivery_decision(
                self,
                &self.paths().repo_root,
                &candidate.base.commit,
                &candidate.candidate.commit,
                &tasks,
                tag,
            )?);
        }
        Ok(decisions)
    }
}
