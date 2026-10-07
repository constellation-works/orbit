//! The `resolves` side effects a task's arrival at done has on its frictions.
//!
//! Only a transition into done resolves the frictions a task names. An update
//! of a task that is already done (a comment, an artifact, a PR sync) must not
//! resolve them again: a human may have reopened one because the fix was
//! incomplete. The exception is a target whose side effect failed: the failure
//! is recorded on the task as a [`RESOLVES_SIDE_EFFECT_FAILED_EVENT`] history
//! entry, and every later write to the done task retries it until a
//! [`RESOLVES_SIDE_EFFECT_RECOVERED_EVENT`] entry supersedes the failure.

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_types::identity::is_valid_friction_id;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskHistoryEntry, TaskRelationType, TaskStatus};

use super::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams};
use crate::OrbitRuntime;

const RELATION_RESOLVES: &str = "resolves";
/// Task history event recording that resolving one `resolves` target failed
/// after the task reached done. Its note's first line is `target=<id>`.
pub(crate) const RESOLVES_SIDE_EFFECT_FAILED_EVENT: &str = "resolves_side_effect_failed";
/// Task history event recording that a retry settled a target an earlier
/// [`RESOLVES_SIDE_EFFECT_FAILED_EVENT`] left pending. Same note shape.
pub(crate) const RESOLVES_SIDE_EFFECT_RECOVERED_EVENT: &str = "resolves_side_effect_recovered";
const TARGET_NOTE_PREFIX: &str = "target=";

impl OrbitRuntime {
    /// Run the `resolves` side effects of a task write that has committed.
    ///
    /// `previous_status` is the status the write replaced. A transition into
    /// done resolves every target; a write to a task that was already done
    /// only retries targets whose side effect failed earlier. The write is
    /// already durable, so a failure here is logged and recorded on the task
    /// for a later retry, never returned to the caller.
    pub(crate) fn record_resolves_side_effects(&self, previous_status: TaskStatus, task: &Task) {
        if task.status != TaskStatus::Done {
            return;
        }
        let targets: Vec<&str> = resolves_targets(task).collect();
        if targets.is_empty() {
            return;
        }
        let pending = match self.get_task_history(&task.id) {
            Ok(history) => pending_resolves_retries(&history),
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id,
                    %error,
                    "reading pending resolves retries failed"
                );
                BTreeSet::new()
            }
        };
        let targets: Vec<&str> = targets
            .into_iter()
            .filter(|target| previous_status != TaskStatus::Done || pending.contains(*target))
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut history = Vec::new();
        for (target, event) in self.apply_resolves_side_effects(task, &targets) {
            match &event {
                OrbitEvent::TaskRelationSideEffectFailed { reason, .. } => {
                    // A target that is still pending keeps its earlier entry;
                    // repeating it on every write would only grow the history.
                    if !pending.contains(target) {
                        history.push(resolves_history_entry(
                            RESOLVES_SIDE_EFFECT_FAILED_EVENT,
                            target,
                            Some(reason),
                        ));
                    }
                }
                _ if pending.contains(target) => history.push(resolves_history_entry(
                    RESOLVES_SIDE_EFFECT_RECOVERED_EVENT,
                    target,
                    None,
                )),
                _ => {}
            }
            if let Err(error) = self.record_event(event) {
                tracing::warn!(task_id = %task.id, %error, "recording a resolves side-effect event failed");
            }
        }
        if !history.is_empty()
            && let Err(error) = self.append_resolves_history(&task.id, history)
        {
            tracing::warn!(
                task_id = %task.id,
                %error,
                "recording the resolves side-effect outcome on the task failed"
            );
        }
    }

    fn apply_resolves_side_effects<'a>(
        &self,
        task: &Task,
        targets: &[&'a str],
    ) -> Vec<(&'a str, OrbitEvent)> {
        let failed = |target: &str, reason: String| OrbitEvent::TaskRelationSideEffectFailed {
            task_id: task.id.clone(),
            target: target.to_string(),
            relation: RELATION_RESOLVES.to_string(),
            reason,
        };
        let frictions = match crate::runtime::friction::store_for(self) {
            Ok(store) => store,
            // Without a store there is no per-relation verdict to give, so
            // report the failure once against each target.
            Err(error) => {
                return targets
                    .iter()
                    .map(|target| (*target, failed(target, error.to_string())))
                    .collect();
            }
        };
        targets
            .iter()
            .map(|target| {
                let event = match frictions.auto_resolve_by_task(target, &task.id, Utc::now()) {
                    Ok(Some(_)) => OrbitEvent::FrictionAutoResolved {
                        task_id: task.id.clone(),
                        friction_id: target.to_string(),
                    },
                    Ok(None) => OrbitEvent::TaskRelationDangling {
                        task_id: task.id.clone(),
                        target: target.to_string(),
                        relation: RELATION_RESOLVES.to_string(),
                    },
                    Err(error) => failed(target, error.to_string()),
                };
                (*target, event)
            })
            .collect()
    }

    fn append_resolves_history(
        &self,
        task_id: &str,
        history: Vec<TaskHistoryEntry>,
    ) -> Result<(), OrbitError> {
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                self.with_mutation(|| {
                    let updated = self.stores().task_records().update(
                        task_id,
                        TaskRecordUpdateParams {
                            actor: SYSTEM_ACTOR_LABEL.to_string(),
                            append_history: history.clone(),
                            ..Default::default()
                        },
                    )?;
                    Ok((
                        updated,
                        OrbitEvent::TaskUpdated {
                            id: task_id.to_string(),
                        },
                    ))
                })?;
                Ok(())
            })
    }
}

/// The friction targets of `task`'s `resolves` relations, in relation order.
fn resolves_targets(task: &Task) -> impl Iterator<Item = &str> {
    task.relations
        .iter()
        .filter(|relation| relation.relation_type == TaskRelationType::Resolves)
        .map(|relation| relation.target.as_str())
        .filter(|target| is_valid_friction_id(target))
}

/// Targets whose latest side-effect history entry records a failure.
fn pending_resolves_retries(history: &[TaskHistoryEntry]) -> BTreeSet<String> {
    let mut pending = BTreeSet::new();
    for entry in history {
        let Some(target) = entry
            .note
            .as_deref()
            .and_then(|note| note.lines().next())
            .and_then(|line| line.strip_prefix(TARGET_NOTE_PREFIX))
        else {
            continue;
        };
        match entry.event.as_str() {
            RESOLVES_SIDE_EFFECT_FAILED_EVENT => {
                pending.insert(target.to_string());
            }
            RESOLVES_SIDE_EFFECT_RECOVERED_EVENT => {
                pending.remove(target);
            }
            _ => {}
        }
    }
    pending
}

fn resolves_history_entry(event: &str, target: &str, reason: Option<&str>) -> TaskHistoryEntry {
    let mut note = format!("{TARGET_NOTE_PREFIX}{target}");
    if let Some(reason) = reason {
        note.push_str("\nreason: ");
        note.push_str(&redact_all(reason));
    }
    TaskHistoryEntry {
        at: Utc::now(),
        by: SYSTEM_ACTOR_LABEL.to_string(),
        event: event.to_string(),
        note: Some(note),
        from_status: None,
        to_status: None,
    }
}
