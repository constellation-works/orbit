//! The `resolves` side effects a task's arrival at done has on its frictions.
//!
//! Only a transition into done resolves the frictions a task names. An update
//! of a task that is already done (a comment, an artifact, a PR sync) must not
//! resolve them again: a human may have reopened one because the fix was
//! incomplete. The exception is a target whose side effect is pending or
//! failed. Final recovery records pending targets with its decision so a crash
//! before the post-lock effects leaves durable retry work. Failures are recorded
//! as [`RESOLVES_SIDE_EFFECT_FAILED_EVENT`] history entries. Later writes to the
//! done task retry unfinished targets until a
//! [`RESOLVES_SIDE_EFFECT_RECOVERED_EVENT`] entry settles them.

use std::collections::BTreeSet;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_store::contracts::FrictionLandingOutcome;
use orbit_types::identity::is_valid_friction_id;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskHistoryEntry, TaskRelationType, TaskStatus};
use serde_json::Value;

use super::{SYSTEM_ACTOR_LABEL, TaskRecordUpdateParams};
use crate::OrbitRuntime;

const RELATION_RESOLVES: &str = "resolves";
/// Task history event recording that resolving one `resolves` target failed
/// after the task reached done. Its note's first line is `target=<id>`.
pub(crate) const RESOLVES_SIDE_EFFECT_FAILED_EVENT: &str = "resolves_side_effect_failed";
const RESOLVES_SIDE_EFFECT_PENDING_EVENT: &str = "resolves_side_effect_pending";
/// Task history event recording that a pending or failed target was settled.
/// Same note shape as [`RESOLVES_SIDE_EFFECT_FAILED_EVENT`].
pub(crate) const RESOLVES_SIDE_EFFECT_RECOVERED_EVENT: &str = "resolves_side_effect_recovered";
const TARGET_NOTE_PREFIX: &str = "target=";
const RESOLVES_LANDING_EVENT: &str = "friction_resolved_on_landing";

/// Evidence retained on the transition that delivered the task. Later retry
/// writes use this same commit and PR, never a freshly observed branch head.
struct LandingResolution {
    commit: String,
    pr_number: Option<u64>,
}

impl OrbitRuntime {
    /// Run the `resolves` side effects of a task write that has committed.
    ///
    /// `previous_status` is the status the write replaced. A transition into
    /// done resolves every target; a write to a task that was already done
    /// only retries targets whose side effect is pending or failed. The write is
    /// already durable, so a failure here is logged and recorded on the task
    /// for a later retry, never returned to the caller.
    pub(crate) fn record_resolves_side_effects(&self, previous_status: TaskStatus, task: &Task) {
        if task.status != TaskStatus::Done {
            return;
        }
        let targets = named_friction_targets(task);
        if targets.is_empty() {
            return;
        }
        let task_history = match self.get_task_history(&task.id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id,
                    %error,
                    "reading pending resolves retries failed"
                );
                Vec::new()
            }
        };
        let pending = pending_resolves_retries(&task_history);
        let landing = landing_resolution(&task_history);
        let relations: BTreeSet<&str> = resolves_targets(task).collect();
        let targets: Vec<&str> = targets
            .iter()
            .map(String::as_str)
            .filter(|target| landing.is_some() || relations.contains(target))
            .filter(|target| previous_status != TaskStatus::Done || pending.contains(*target))
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut history = Vec::new();
        for (target, event) in self.apply_resolves_side_effects(task, &targets, landing.as_ref()) {
            let Some(event) = event else {
                if pending.contains(target) {
                    history.push(resolves_history_entry(
                        RESOLVES_SIDE_EFFECT_RECOVERED_EVENT,
                        target,
                        None,
                    ));
                }
                continue;
            };
            match &event {
                // A target that is still pending keeps its earlier entry;
                // repeating it on every write would only grow the history.
                OrbitEvent::TaskRelationSideEffectFailed { reason, .. }
                    if !pending.contains(target) =>
                {
                    history.push(resolves_history_entry(
                        RESOLVES_SIDE_EFFECT_FAILED_EVENT,
                        target,
                        Some(reason),
                    ));
                }
                OrbitEvent::TaskRelationSideEffectFailed { .. } => {}
                _ if pending.contains(target) => history.push(resolves_history_entry(
                    RESOLVES_SIDE_EFFECT_RECOVERED_EVENT,
                    target,
                    None,
                )),
                _ => {}
            }
            if matches!(event, OrbitEvent::FrictionAutoResolved { .. })
                && let Some(landing) = &landing
            {
                let mut entry = resolves_history_entry(RESOLVES_LANDING_EVENT, target, None);
                entry.note = Some(format!(
                    "{TARGET_NOTE_PREFIX}{target}\ncommit={}\npr={}",
                    landing.commit,
                    landing
                        .pr_number
                        .map_or_else(|| "none".to_string(), |number| format!("#{number}")),
                ));
                history.push(entry);
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
        landing: Option<&LandingResolution>,
    ) -> Vec<(&'a str, Option<OrbitEvent>)> {
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
                    .map(|target| (*target, Some(failed(target, error.to_string()))))
                    .collect();
            }
        };
        targets
            .iter()
            .map(|target| {
                let result = match landing {
                    Some(landing) => frictions.auto_resolve_by_landing(
                        target,
                        &task.id,
                        &landing.commit,
                        landing.pr_number,
                        Utc::now(),
                    ),
                    None => frictions
                        .auto_resolve_by_task(target, &task.id, Utc::now())
                        .map(|record| {
                            if record.is_some() {
                                FrictionLandingOutcome::Resolved
                            } else {
                                FrictionLandingOutcome::Missing
                            }
                        }),
                };
                let event = match result {
                    Ok(FrictionLandingOutcome::AlreadyResolved) => return (*target, None),
                    Ok(FrictionLandingOutcome::Resolved) => OrbitEvent::FrictionAutoResolved {
                        task_id: task.id.clone(),
                        friction_id: target.to_string(),
                    },
                    Ok(FrictionLandingOutcome::Missing) => OrbitEvent::TaskRelationDangling {
                        task_id: task.id.clone(),
                        target: target.to_string(),
                        relation: RELATION_RESOLVES.to_string(),
                    },
                    Err(error) => failed(target, error.to_string()),
                };
                (*target, Some(event))
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

/// Landing closes every explicitly named local friction. Scan description and
/// criteria only, not titles, comments, plans or summaries; delimiters keep a
/// prefix of an unrelated identifier from being interpreted as a target.
fn named_friction_targets(task: &Task) -> BTreeSet<String> {
    let mut targets: BTreeSet<String> = resolves_targets(task).map(str::to_string).collect();
    for text in std::iter::once(task.description.as_str())
        .chain(task.acceptance_criteria.iter().map(String::as_str))
    {
        for token in text.split(|ch: char| ch != '-' && ch != '_' && !ch.is_alphanumeric()) {
            if is_valid_friction_id(token) {
                targets.insert(token.to_string());
            }
        }
    }
    targets
}

fn landing_resolution(history: &[TaskHistoryEntry]) -> Option<LandingResolution> {
    // Only the latest transition into done counts: an older delivery cannot
    // authorize resolution after a task was reopened and completed again.
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Done))?;
    let note = entry.note.as_deref()?;
    if entry.event == "landing_completed" {
        let value: Value = serde_json::from_str(note).ok()?;
        let resolution = value.get("friction_resolution")?;
        return Some(LandingResolution {
            commit: resolution.get("commit")?.as_str()?.to_string(),
            pr_number: resolution.get("pr_number").and_then(Value::as_u64),
        });
    }
    // The ordinary owner PR completion path already retains this verified
    // provider evidence in its durable authorization note.
    let (_, delivery) = note.rsplit_once("; delivered by pull request #")?;
    let (pr, commit) = delivery.split_once(" merged as ")?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(LandingResolution {
        commit: commit.to_string(),
        pr_number: Some(pr.parse().ok()?),
    })
}

/// Targets whose latest side-effect history entry records unfinished work.
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
            RESOLVES_SIDE_EFFECT_PENDING_EVENT | RESOLVES_SIDE_EFFECT_FAILED_EVENT => {
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

/// Retry work persisted with a final-recovery transition into done, before
/// releasing its task lock or reading the task for the side effects.
pub(super) fn pending_resolves_history(task: &Task) -> Vec<TaskHistoryEntry> {
    resolves_targets(task)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|target| resolves_history_entry(RESOLVES_SIDE_EFFECT_PENDING_EVENT, target, None))
        .collect()
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
        operation_id: None,
    }
}
