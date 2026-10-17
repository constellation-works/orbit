//! One repair task per red base failure [ORB-15374].
//!
//! A baseline-red hold waits for the base to turn green, and nothing else
//! files that work. The owner's clock tick, which already reads every
//! standing hold ([`OrbitRuntime::run_baseline_hold_tick`]), makes sure each
//! hold that has not lifted has one open repair task for its command and
//! failure set in the workspace:
//!
//! - the first hold for that pair files the task, naming the command, the
//!   failures, the first red base and the held task;
//! - every later hold with the same failures, on another task or a later
//!   base, links to it instead: a `related_to` relation on the repair task
//!   and a comment naming the held task and its base.
//!
//! The pair is keyed by a digest of the command and the failure set, stored
//! as the repair task's [`BASELINE_RED_REF_SYSTEM`] external ref, so finding
//! it is an exact lookup rather than a text search. Only an open repair task
//! dedupes: once it is done or rejected, a hold that still names the same
//! failures files a new one. A hold that records no failures (one written
//! before holds kept them) files nothing.
//!
//! Filing runs only in the tick, which holds the host's sweep lock, so two
//! ticks never file for one pair at once; the tick stands down on claimed
//! workers and replica checkouts, so only the owner files. The
//! `run-failure-patterns` sweep builds its `base_red` patterns from these
//! same holds and searches open tasks before filing, so it finds the repair
//! task the tick filed first.

use std::collections::HashMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::security::release::sha256_hex;
use orbit_types::task::{
    ExternalRef, Task, TaskComment, TaskComplexity, TaskPriority, TaskRelation, TaskRelationType,
    TaskStatus, TaskType,
};
use orbit_types::workflow::BaselineRedHold;

use super::{TaskAddParams, TaskRecordUpdateParams};
use crate::OrbitRuntime;

/// External ref system that keys a repair task by its command and failures.
const BASELINE_RED_REF_SYSTEM: &str = "baseline-red";
/// Tag on every repair task the tick files.
const BASELINE_RED_REPAIR_TAG: &str = "baseline-red-repair";
/// Hex digits of the key digest kept in the external ref.
const KEY_DIGITS: usize = 24;

/// What one tick did about one hold's repair task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RepairLink {
    /// Filed a new repair task, with this id.
    Filed(String),
    /// Linked the held task to the open repair task with this id.
    Linked(String),
    /// The open repair task already names the held task, or the hold records
    /// no failures to key one by.
    Unchanged,
}

/// The open repair task for each key, as one tick found or filed it.
#[derive(Default)]
pub(super) struct RepairIndex(HashMap<String, Option<String>>);

impl OrbitRuntime {
    /// Make sure `hold`'s command and failure set has one open repair task
    /// that names `held`: file it when none is open, link `held` to it
    /// otherwise.
    pub(super) fn link_baseline_red_repair(
        &self,
        held: &Task,
        hold: &BaselineRedHold,
        index: &mut RepairIndex,
    ) -> Result<RepairLink, OrbitError> {
        let Some(key) = repair_key(hold) else {
            return Ok(RepairLink::Unchanged);
        };
        let repair_id = match index.0.get(&key) {
            Some(found) => found.clone(),
            None => {
                let found = self.open_repair_task(&key)?;
                index.0.insert(key.clone(), found.clone());
                found
            }
        };
        match repair_id {
            Some(repair_id) if repair_id == held.id => Ok(RepairLink::Unchanged),
            Some(repair_id) => Ok(if self.link_held_task(&repair_id, held, hold)? {
                RepairLink::Linked(repair_id)
            } else {
                RepairLink::Unchanged
            }),
            None => {
                let repair = self.add_task(repair_task_params(&key, held, hold)?)?;
                tracing::info!(
                    task_id = %held.id,
                    repair_id = %repair.id,
                    command = hold.command,
                    "filed a repair task for a red base"
                );
                index.0.insert(key, Some(repair.id.clone()));
                Ok(RepairLink::Filed(repair.id))
            }
        }
    }

    /// The open repair task keyed `key`, if any.
    fn open_repair_task(&self, key: &str) -> Result<Option<String>, OrbitError> {
        let reference = repair_ref(key)?;
        Ok(self
            .list_tasks_filtered(None, None, None, None, Some(&reference), None)?
            .into_iter()
            .filter(|task| !task.status.is_terminal())
            .min_by(|left, right| left.created_at.cmp(&right.created_at))
            .map(|task| task.id))
    }

    /// Relate `held` to the repair task and comment its hold, unless the
    /// repair task already relates it. `true` when it was linked here.
    fn link_held_task(
        &self,
        repair_id: &str,
        held: &Task,
        hold: &BaselineRedHold,
    ) -> Result<bool, OrbitError> {
        let mut linked = false;
        self.stores()
            .tasks()
            .with_task_write_lock(repair_id, &mut || {
                let repair = self.get_task(repair_id)?;
                if repair.status.is_terminal()
                    || repair.relations.iter().any(|relation| {
                        relation.relation_type == TaskRelationType::RelatedTo
                            && relation.target == held.id
                    })
                {
                    return Ok(());
                }
                let mut relations = repair.relations.clone();
                relations.push(TaskRelation {
                    relation_type: TaskRelationType::RelatedTo,
                    target: held.id.clone(),
                });
                self.stores().task_records().update(
                    repair_id,
                    TaskRecordUpdateParams {
                        actor: "system".into(),
                        relations: Some(relations),
                        append_comments: vec![TaskComment {
                            at: Utc::now(),
                            by: "system".into(),
                            message: format!(
                                "The same red base failure also holds {} (\"{}\"): required \
                                 validation `{}` fails on base `{}`, run `{}`.",
                                held.id, held.title, hold.command, hold.base_sha, hold.run_id
                            ),
                            operation_id: None,
                        }],
                        ..Default::default()
                    },
                )?;
                linked = true;
                Ok(())
            })?;
        Ok(linked)
    }
}

/// The digest keying `hold`'s command and failure set, or `None` when the
/// hold records no failures.
fn repair_key(hold: &BaselineRedHold) -> Option<String> {
    let failures = &hold.failures;
    if failures.is_empty() {
        return None;
    }
    let identity = match &failures.identity_digest {
        Some(digest) => format!("{}\0identity-digest-v1\0{digest}", hold.command.trim()),
        // Keep the exact persisted key for older holds and output-only
        // failures, whose complete identity set cannot be recovered.
        None => format!(
            "{}\0{}\0{}\0{}",
            hold.command.trim(),
            failures.identities.join("\n"),
            failures.omitted,
            failures.tail.join("\n")
        ),
    };
    Some(sha256_hex(identity.as_bytes())[..KEY_DIGITS].to_string())
}

fn repair_ref(key: &str) -> Result<ExternalRef, OrbitError> {
    ExternalRef::try_new(BASELINE_RED_REF_SYSTEM.to_string(), key.to_string(), None)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

/// The repair task the first hold for a key files.
fn repair_task_params(
    key: &str,
    held: &Task,
    hold: &BaselineRedHold,
) -> Result<TaskAddParams, OrbitError> {
    let failures = &hold.failures;
    let base_ref = if hold.base_ref.trim().is_empty() {
        "the base".to_string()
    } else {
        format!("`{}`", hold.base_ref.trim())
    };
    let subject = match failures.identities.first() {
        Some(first) => {
            let name = first.strip_prefix("test ").unwrap_or(first);
            let more = failures.identities.len() - 1 + failures.omitted;
            if more == 0 {
                format!("`{name}`")
            } else {
                format!("`{name}` and {more} more")
            }
        }
        None => format!("`{}`", hold.command),
    };
    let listed = if failures.identities.is_empty() {
        format!(
            "The base run names no failing test. Its output ends:\n\n```text\n{}\n```\n",
            failures.tail.join("\n")
        )
    } else {
        let mut listed = failures
            .identities
            .iter()
            .map(|identity| format!("- `{identity}`"))
            .collect::<Vec<_>>();
        if failures.omitted > 0 {
            listed.push(format!("- and {} more", failures.omitted));
        }
        listed.join("\n") + "\n"
    };
    let description = format!(
        "## Red base\n\n\
         Required validation `{command}` fails on {base_ref} exactly as it fails on the \
         candidates it gates, so delivery holds those tasks in the backlog until the command \
         passes on a new base tip. The candidates did not cause the failure; this task makes the \
         base green again.\n\n\
         - Command: `{command}`\n\
         - First red base: `{base_sha}`\n\
         - Run that found it: `{run_id}`\n\
         - Held tasks: {held_id} (each later hold with the same failures is added as a related \
         task and a comment)\n\n\
         ## Failing on the base\n\n{listed}\n\
         The base run's log is attached to each held task as \
         `validation/<run_id>/<n>.baseline.json`.\n",
        command = hold.command,
        base_sha = hold.base_sha,
        run_id = hold.run_id,
        held_id = held.id,
    );
    Ok(TaskAddParams {
        title: format!("Repair the red base: {subject} fails on {base_ref}"),
        description,
        acceptance_criteria: vec![
            format!(
                "Required validation `{}` passes on a new {base_ref} tip, so the holds on the \
                 related tasks lift",
                hold.command
            ),
            "Each failure listed in the description is fixed at its cause, or its removal from \
             the suite is justified in the change"
                .to_string(),
        ],
        relations: vec![TaskRelation {
            relation_type: TaskRelationType::RelatedTo,
            target: held.id.clone(),
        }],
        tags: vec![BASELINE_RED_REPAIR_TAG.to_string()],
        priority: TaskPriority::High,
        complexity: TaskComplexity::Medium,
        task_type: Some(TaskType::Bug),
        status: Some(TaskStatus::Backlog),
        system_created: true,
        external_refs: vec![repair_ref(key)?],
        ..TaskAddParams::default()
    })
}
