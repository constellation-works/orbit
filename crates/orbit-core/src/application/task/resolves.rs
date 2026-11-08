//! The `resolves` side effects a task's arrival at done has on its frictions.
//!
//! Only a transition into done resolves the frictions a task names. An update
//! of a task that is already done (a comment, an artifact, a PR sync) must not
//! resolve them again: a human may have reopened one because the fix was
//! incomplete. The exception is a target whose side effect is pending or
//! failed. Every transition into done commits a pending entry for each named
//! target, relation or text alike, so a crash or unreadable history before
//! these post-commit effects leaves durable retry work. Failures are recorded
//! as [`RESOLVES_SIDE_EFFECT_FAILED_EVENT`] history entries. Later writes to the
//! done task retry unfinished targets, with the transition's own landing
//! evidence, until a [`RESOLVES_SIDE_EFFECT_RECOVERED_EVENT`] entry settles them.

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_all;
use orbit_store::contracts::FrictionLandingOutcome;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    RESOLVES_SIDE_EFFECT_PENDING_EVENT, RESOLVES_TARGET_NOTE_PREFIX, Task, TaskHistoryEntry,
    TaskStatus, named_friction_targets, pending_resolves_entries, resolves_relation_targets,
};
use serde_json::Value;

use super::{SYSTEM_ACTOR_LABEL, TaskCodebasePort, TaskNoticePort, TaskRecordUpdateParams};
use crate::OrbitRuntime;
use crate::application::task::TaskRecords;
use crate::application::task::final_recovery::FINAL_RECOVERY_COMPLETED_EVENT;

const RELATION_RESOLVES: &str = "resolves";
/// Task history event recording that resolving one `resolves` target failed
/// after the task reached done. Its note's first line is `target=<id>`.
pub(crate) const RESOLVES_SIDE_EFFECT_FAILED_EVENT: &str = "resolves_side_effect_failed";
/// Task history event recording that a pending or failed target was settled.
/// Same note shape as [`RESOLVES_SIDE_EFFECT_FAILED_EVENT`].
pub(crate) const RESOLVES_SIDE_EFFECT_RECOVERED_EVENT: &str = "resolves_side_effect_recovered";
const RESOLVES_LANDING_EVENT: &str = "friction_resolved_on_landing";

#[cfg(test)]
thread_local! {
    pub(super) static FAIL_HISTORY_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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
    /// for a later retry, never returned to the caller. Unreadable history
    /// resolves nothing: without the landing evidence a target could only be
    /// dropped or resolved with less, and its committed pending entry keeps it.
    pub(crate) fn record_resolves_side_effects(&self, previous_status: TaskStatus, task: &Task) {
        if task.status != TaskStatus::Done {
            return;
        }
        // Every writer of a transition into done runs these effects, so the
        // landing it names records its diff stats here too [ORB-18383].
        self.record_landed_diff_stats_on_done(previous_status, task);
        // ...and closes the fleet notices linked to the task.
        if previous_status != TaskStatus::Done {
            self.close_linked_notices_for_done_transition(task.id.as_str());
        }
        let task_history = match self.resolves_task_history(&task.id) {
            Ok(history) => history,
            Err(error) => {
                tracing::warn!(
                    task_id = %task.id,
                    %error,
                    "reading pending resolves retries failed; the next write retries them"
                );
                return;
            }
        };
        let pending = pending_resolves_retries(&task_history);
        let landing = landing_resolution(&task_history);
        let relations: BTreeSet<&str> = resolves_relation_targets(&task.relations).collect();
        // What is pending stays owed even if the task stopped naming it.
        let mut named: BTreeSet<String> = pending.keys().cloned().collect();
        if previous_status != TaskStatus::Done {
            named.extend(task_friction_targets(task));
        }
        // Only a landing authorizes a target named by text alone.
        let (targets, unauthorized): (Vec<&str>, Vec<&str>) = named
            .iter()
            .map(String::as_str)
            .partition(|target| landing.is_some() || relations.contains(target));
        let mut history: Vec<TaskHistoryEntry> = unauthorized
            .into_iter()
            .filter(|target| pending.contains_key(*target))
            .map(|target| {
                resolves_history_entry(
                    RESOLVES_SIDE_EFFECT_RECOVERED_EVENT,
                    target,
                    Some("no landing evidence authorizes a target named only in task text"),
                )
            })
            .collect();
        for (target, event) in self.apply_resolves_side_effects(task, &targets, landing.as_ref()) {
            let Some(event) = event else {
                if pending.contains_key(target) {
                    history.push(resolves_history_entry(
                        RESOLVES_SIDE_EFFECT_RECOVERED_EVENT,
                        target,
                        None,
                    ));
                }
                continue;
            };
            match &event {
                // A target already recorded as failed keeps its earlier entry;
                // repeating it on every write would only grow the history.
                OrbitEvent::TaskRelationSideEffectFailed { reason, .. }
                    if pending.get(target) != Some(&UnfinishedResolve::Failed) =>
                {
                    history.push(resolves_history_entry(
                        RESOLVES_SIDE_EFFECT_FAILED_EVENT,
                        target,
                        Some(reason),
                    ));
                }
                OrbitEvent::TaskRelationSideEffectFailed { .. } => {}
                _ if pending.contains_key(target) => history.push(resolves_history_entry(
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
                    "{RESOLVES_TARGET_NOTE_PREFIX}{target}\ncommit={}\npr={}",
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
            && let Err(error) = self.append_system_history(&task.id, history)
        {
            tracing::warn!(
                task_id = %task.id,
                %error,
                "recording the resolves side-effect outcome on the task failed"
            );
        }
    }

    fn resolves_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        #[cfg(test)]
        if FAIL_HISTORY_READ.with(|fail| fail.replace(false)) {
            return Err(OrbitError::Execution(
                "injected resolves history read failure".to_string(),
            ));
        }
        self.get_task_history(task_id)
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

    /// Append system-authored side-effect bookkeeping to a task's history.
    pub(in crate::application) fn append_system_history(
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

/// Landing closes every explicitly named local friction. Scan description and
/// criteria only, not titles, comments, plans or summaries.
fn task_friction_targets(task: &Task) -> BTreeSet<String> {
    named_friction_targets(
        &task.relations,
        std::iter::once(task.description.as_str())
            .chain(task.acceptance_criteria.iter().map(String::as_str)),
    )
}

fn landing_resolution(history: &[TaskHistoryEntry]) -> Option<LandingResolution> {
    // Only the latest transition into done counts: an older delivery cannot
    // authorize resolution after a task was reopened and completed again.
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Done))?;
    let note = entry.note.as_deref()?;
    if entry.event == FINAL_RECOVERY_COMPLETED_EVENT {
        return Some(LandingResolution {
            commit: covering_commit_delivery(note)?.to_string(),
            pr_number: None,
        });
    }
    if entry.event == "landing_completed" {
        let value: Value = serde_json::from_str(note).ok()?;
        let resolution = value.get("friction_resolution")?;
        return Some(LandingResolution {
            commit: resolution.get("commit")?.as_str()?.to_string(),
            pr_number: resolution.get("pr_number").and_then(Value::as_u64),
        });
    }
    let (pr_number, commit) = pull_request_delivery(note)?;
    Some(LandingResolution {
        commit: commit.to_string(),
        pr_number: Some(pr_number),
    })
}

/// The pull request and full merge commit a done transition's note records
/// as `; delivered by pull request #N merged as <sha>`. The ordinary owner PR
/// completion path retains this verified provider evidence in its durable
/// authorization note; `None` for any other note or an abbreviated commit.
pub(crate) fn pull_request_delivery(note: &str) -> Option<(u64, &str)> {
    let (_, delivery) = note.rsplit_once("; delivered by pull request #")?;
    let (pr, commit) = delivery.split_once(" merged as ")?;
    if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some((pr.parse().ok()?, commit))
}

/// How far an unfinished target got.
#[derive(PartialEq)]
enum UnfinishedResolve {
    /// Committed with the transition; no attempt has been recorded.
    Pending,
    /// An attempt failed and its reason is on the task.
    Failed,
}

/// The commit a final recovery's completion note verified on its base, as
/// `covering commit <sha> is reachable`, retained like an already-landed
/// handoff's covering commit.
fn covering_commit_delivery(note: &str) -> Option<&str> {
    let (_, covering) = note.split_once("covering commit ")?;
    let (commit, _) = covering.split_once(' ')?;
    (commit.len() >= 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(commit)
}

/// Targets whose latest side-effect history entry records unfinished work.
fn pending_resolves_retries(history: &[TaskHistoryEntry]) -> BTreeMap<String, UnfinishedResolve> {
    let mut pending = BTreeMap::new();
    for entry in history {
        let Some(target) = entry
            .note
            .as_deref()
            .and_then(|note| note.lines().next())
            .and_then(|line| line.strip_prefix(RESOLVES_TARGET_NOTE_PREFIX))
        else {
            continue;
        };
        match entry.event.as_str() {
            RESOLVES_SIDE_EFFECT_PENDING_EVENT => {
                pending.insert(target.to_string(), UnfinishedResolve::Pending);
            }
            RESOLVES_SIDE_EFFECT_FAILED_EVENT => {
                pending.insert(target.to_string(), UnfinishedResolve::Failed);
            }
            RESOLVES_SIDE_EFFECT_RECOVERED_EVENT => {
                pending.remove(target);
            }
            _ => {}
        }
    }
    pending
}

/// Retry work for a write that bypasses the store's history commit: the
/// desktop completion's own atomic mutation appends these with its transition.
pub(super) fn pending_resolves_history(task: &Task) -> Vec<TaskHistoryEntry> {
    pending_resolves_entries(&task_friction_targets(task), SYSTEM_ACTOR_LABEL, Utc::now())
}

fn resolves_history_entry(event: &str, target: &str, reason: Option<&str>) -> TaskHistoryEntry {
    let mut note = format!("{RESOLVES_TARGET_NOTE_PREFIX}{target}");
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
        decision: None,
    }
}
