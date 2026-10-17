//! Blocks caused by Orbit's own transient infrastructure, and their requeue.
//!
//! Final recovery classifies a deterministic step that lost only to a busy
//! lock or a locked store without an agent (see
//! [`orbit_types::workflow::TransientInfrastructure`]) and records the block
//! it applies with a [`TRANSIENT_INFRA_BLOCKED_EVENT`]. Such a task is
//! *transient-infra blocked* while that record names its current run, the
//! run's escalation is still its latest final-recovery decision, and nobody
//! but Orbit's own automation changed its status since.
//!
//! The cleared-infra requeue (`orbit task recheck-blocked --confirm`, and
//! the owner's clock tick) returns such a task to the backlog through the
//! same records an operator requeue writes, so the next run resumes the
//! candidate its failure handoff published. It acts only when:
//!
//! - the task's run has conclusively stopped, so its failure handoff has
//!   settled (final recovery blocks the task before the handoff publishes);
//! - nobody but Orbit's automation commented on or wrote to the task since
//!   the block: a human's attributed call stands;
//! - no earlier attempt on this block found that the publication does not
//!   authenticate ([`TRANSIENT_INFRA_REQUEUE_REFUSED_EVENT`]), so such a
//!   publication costs one forge round trip, not one per tick. An attempt
//!   that failed on a transient error instead (the remote, the forge, the
//!   store; [`TRANSIENT_INFRA_REQUEUE_DEFERRED_EVENT`]) is retried no sooner
//!   than `DEFERRAL_BACKOFF_MINUTES` later, at most
//!   `MAX_DEFERRALS` times;
//! - the probe passes: the task store takes an immediate write transaction
//!   now, and the task's own write lock is then free. The probe cannot take
//!   the lock that actually timed out, which may have been another file
//!   lock or a remote owner's; it shows only that the task's own
//!   coordination is usable again.
//!
//! The publication is authenticated before the task write lock is taken,
//! since that asks the remote and the forge; under the lock only the block's
//! identity and the task's last write are re-checked.
//!
//! Each publication (or, unpublished, each task) takes at most
//! [`MAX_TRANSIENT_INFRA_REQUEUES`] such requeues; after that the block stays
//! for an operator, so a condition that keeps recurring cannot loop.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    MAX_TRANSIENT_INFRA_REQUEUES, TRANSIENT_INFRA_BLOCKED_EVENT,
    TRANSIENT_INFRA_REQUEUE_DEFERRED_EVENT, TRANSIENT_INFRA_REQUEUE_REFUSED_EVENT,
    TRANSIENT_INFRA_REQUEUED_EVENT, TransientInfraBlock, TransientInfraRequeue,
    TransientInfrastructure,
};
use rusqlite::TransactionBehavior;

use super::TaskRecordUpdateParams as StoreTaskUpdateParams;

/// Transient authentication errors one block may take before it waits for an
/// operator.
const MAX_DEFERRALS: usize = 5;

/// Minutes the cleared-infra requeue waits after a transient authentication
/// error before it tries the same block again.
const DEFERRAL_BACKOFF_MINUTES: i64 = 10;
use super::helpers::{SYSTEM_ACTOR_LABEL, is_automation_actor};
use super::requeue::CandidateRequeue;
use super::transitions::INFRA_BLOCK_CLEARED_EVENT;
use crate::OrbitRuntime;

/// A blocked task whose block is transient Orbit infrastructure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransientInfraBlockedTask {
    pub task_id: String,
    pub title: String,
    /// When the block was classified; identifies it, so a requeue can
    /// confirm it is still the one it scanned.
    pub blocked_at: DateTime<Utc>,
    pub run_id: String,
    pub failed_step_id: String,
    pub class: TransientInfrastructure,
    /// The pull request the run's failure handoff published, if any.
    pub pull_request: Option<String>,
    /// Requeues this publication already took for transient infrastructure.
    pub requeues: usize,
    /// The bound is spent: only an operator requeues it now.
    pub exhausted: bool,
    /// Whether the probe passed when the task was classified.
    pub cleared: bool,
    /// Why the cleared-infra requeue leaves this block alone even once the
    /// probe passes: the run has not stopped, a human acted after the block,
    /// or an earlier attempt could not authenticate the publication.
    pub held: Option<String>,
}

impl OrbitRuntime {
    /// Blocked tasks in this workspace whose block is transient
    /// infrastructure, each with whether its probe passes now.
    pub fn transient_infra_blocked_tasks(
        &self,
    ) -> Result<Vec<TransientInfraBlockedTask>, OrbitError> {
        let blocked =
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?;
        let mut found = Vec::new();
        for task in blocked {
            if let Some(block) = self.transient_infra_block_of(&task.id)? {
                found.push(block);
            }
        }
        Ok(found)
    }

    fn transient_infra_block_of(
        &self,
        task_id: &str,
    ) -> Result<Option<TransientInfraBlockedTask>, OrbitError> {
        let task = self.get_task(task_id)?;
        let Some(run_id) = task.job_run_id.clone() else {
            return Ok(None);
        };
        if task.status != TaskStatus::Blocked {
            return Ok(None);
        }
        let history = self.get_task_history(task_id)?;
        let Some((index, entry, block)) = history.iter().enumerate().rev().find_map(|(i, e)| {
            (e.event == TRANSIENT_INFRA_BLOCKED_EVENT && e.by == SYSTEM_ACTOR_LABEL)
                .then(|| serde_json::from_str::<TransientInfraBlock>(e.note.as_deref()?).ok())
                .flatten()
                .filter(|block| block.schema_version == 1 && block.run_id == run_id)
                .map(|block| (i, e, block))
        }) else {
            return Ok(None);
        };
        if !untouched_since(&history[index + 1..])
            || latest_final_recovery(&history, &run_id) != Some("escalated")
        {
            return Ok(None);
        }
        let held = match self.unsettled_run(&task)? {
            Some(reason) => Some(reason),
            None => self.block_hold(&task, entry.at, &history[index + 1..])?,
        };
        let pull_request = self
            .handoff_receipt(&task)?
            .map(|receipt| receipt.pull_request);
        let requeues = history
            .iter()
            .filter(|e| e.event == TRANSIENT_INFRA_REQUEUED_EVENT && e.by == SYSTEM_ACTOR_LABEL)
            .filter_map(|e| serde_json::from_str::<TransientInfraRequeue>(e.note.as_deref()?).ok())
            .filter(|requeue| requeue.pull_request == pull_request)
            .count();
        Ok(Some(TransientInfraBlockedTask {
            task_id: task.id.clone(),
            title: task.title.clone(),
            blocked_at: entry.at,
            run_id,
            failed_step_id: block.failed_step_id,
            class: block.class,
            pull_request,
            requeues,
            exhausted: requeues >= MAX_TRANSIENT_INFRA_REQUEUES,
            cleared: self.transient_infrastructure_clear(),
            held,
        }))
    }

    /// Why a human's or an earlier attempt's word stands over this block:
    /// a write or comment after it by someone other than Orbit's automation,
    /// or a recorded refusal to authenticate its publication.
    fn block_hold(
        &self,
        task: &Task,
        blocked_at: DateTime<Utc>,
        after: &[TaskHistoryEntry],
    ) -> Result<Option<String>, OrbitError> {
        if let Some(refused) = after.iter().rev().find(|entry| {
            entry.event == TRANSIENT_INFRA_REQUEUE_REFUSED_EVENT && entry.by == SYSTEM_ACTOR_LABEL
        }) {
            return Ok(Some(format!(
                "an automatic requeue could not authenticate the publication: {}",
                refused.note.as_deref().unwrap_or("no reason recorded")
            )));
        }
        let deferrals = after
            .iter()
            .filter(|entry| {
                entry.event == TRANSIENT_INFRA_REQUEUE_DEFERRED_EVENT
                    && entry.by == SYSTEM_ACTOR_LABEL
            })
            .collect::<Vec<_>>();
        if let Some(last) = deferrals.last() {
            let note = last.note.as_deref().unwrap_or("no reason recorded");
            if deferrals.len() >= MAX_DEFERRALS {
                return Ok(Some(format!(
                    "authenticating the publication failed {} times on transient errors; last: \
                     {note}",
                    deferrals.len()
                )));
            }
            if Utc::now() - last.at < chrono::Duration::minutes(DEFERRAL_BACKOFF_MINUTES) {
                return Ok(Some(format!(
                    "retrying after a transient error at {}: {note}",
                    last.at.to_rfc3339()
                )));
            }
        }
        let comments = self.get_task_comments(&task.id)?;
        let human = after
            .iter()
            .map(|entry| entry.by.as_str())
            .chain(
                comments
                    .iter()
                    .filter(|comment| comment.at > blocked_at)
                    .map(|comment| comment.by.as_str()),
            )
            .find(|by| !is_automation_actor(by));
        Ok(human.map(|by| format!("{by} acted on the task after the block; their call stands")))
    }

    /// The probe: the task store takes an immediate write transaction now.
    /// A requeue then also has to take the task's write lock.
    fn transient_infrastructure_clear(&self) -> bool {
        self.sqlite_store()
            .and_then(|store| {
                store.with_transaction_behavior(TransactionBehavior::Immediate, |_| Ok(()))
            })
            .is_ok()
    }

    /// Return every transient-infra blocked task whose probe passes, whose
    /// bound is not spent and that nothing holds to the backlog, onto its
    /// published candidate. A task whose lock is still held, or that changed
    /// since the scan, is left blocked; one whose publication no longer
    /// authenticates is left blocked with that refusal recorded.
    ///
    /// Stands down off the coordination owner, like every task write.
    pub fn requeue_cleared_transient_infra_blocks(
        &self,
    ) -> Result<Vec<TransientInfraBlockedTask>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut requeued = Vec::new();
        for scanned in self.transient_infra_blocked_tasks()? {
            if !scanned.cleared || scanned.exhausted || scanned.held.is_some() {
                continue;
            }
            match self.requeue_transient_infra_block(&scanned) {
                Ok(cleared) => requeued.extend(cleared),
                Err(error) => tracing::warn!(
                    target: "orbit.core.transient_infra",
                    task_id = %scanned.task_id,
                    error = %error,
                    "left a transient-infrastructure block in place"
                ),
            }
        }
        Ok(requeued)
    }

    fn requeue_transient_infra_block(
        &self,
        scanned: &TransientInfraBlockedTask,
    ) -> Result<Option<TransientInfraBlockedTask>, OrbitError> {
        let task = self.get_task(&scanned.task_id)?;
        // Authenticate first, outside the lock: it asks the remote and forge.
        let preserved = match &scanned.pull_request {
            Some(_) => match self.failure_handoff_requeue_record(&task) {
                Ok((entry, _)) => Some(entry),
                // Only the authentication's own refusal is final; a remote,
                // forge or store error says nothing about the publication.
                Err(error @ OrbitError::JobValidation(_)) => {
                    self.refuse_transient_infra_requeue(scanned, &error.to_string())?;
                    return Ok(None);
                }
                Err(error) => {
                    self.record_transient_infra_attempt(
                        scanned,
                        TRANSIENT_INFRA_REQUEUE_DEFERRED_EVENT,
                        &error.to_string(),
                    )?;
                    return Ok(None);
                }
            },
            None if self.run_published_through_pipeline(&scanned.run_id)? => {
                self.refuse_transient_infra_requeue(
                    scanned,
                    "the run published through its own pipeline steps with no failure-handoff \
                     receipt to authenticate",
                )?;
                return Ok(None);
            }
            None if self.unbound_publication_on_record(&task)? => {
                self.refuse_transient_infra_requeue(
                    scanned,
                    "a pull request or preserved candidate is on record that no failure-handoff \
                     receipt binds",
                )?;
                return Ok(None);
            }
            None => None,
        };
        let mut cleared = None;
        self.stores()
            .tasks()
            .with_task_write_lock(&scanned.task_id, &mut || {
                let Some(current) = self.transient_infra_block_of(&scanned.task_id)? else {
                    return Ok(());
                };
                if current.blocked_at != scanned.blocked_at
                    || current.pull_request != scanned.pull_request
                    || !current.cleared
                    || current.exhausted
                    || current.held.is_some()
                    || self.get_task(&current.task_id)?.updated_at != task.updated_at
                {
                    return Ok(());
                }
                let note = TransientInfraRequeue {
                    schema_version: 1,
                    run_id: current.run_id.clone(),
                    class: current.class,
                    pull_request: current.pull_request.clone(),
                };
                let note = serde_json::to_string(&note).map_err(|error| {
                    OrbitError::Execution(format!("encode infra requeue: {error}"))
                })?;
                self.write_candidate_requeue(
                    &task,
                    CandidateRequeue {
                        actor: SYSTEM_ACTOR_LABEL.to_string(),
                        status_event: INFRA_BLOCK_CLEARED_EVENT,
                        status_note: format!(
                            "transient infrastructure ({}) cleared: the probe passed, so the \
                             block recorded at {} (run_id={}) no longer reproduces; requeue {} \
                             of {}",
                            current.class.as_str(),
                            current.blocked_at.to_rfc3339(),
                            current.run_id,
                            current.requeues + 1,
                            MAX_TRANSIENT_INFRA_REQUEUES,
                        ),
                        decision: "infra_requeue",
                        comment: None,
                        preserved: preserved.clone(),
                        history: vec![TaskHistoryEntry {
                            at: Utc::now(),
                            by: SYSTEM_ACTOR_LABEL.to_string(),
                            event: TRANSIENT_INFRA_REQUEUED_EVENT.to_string(),
                            note: Some(note),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                    },
                )?;
                cleared = Some(current);
                Ok(())
            })?;
        Ok(cleared)
    }

    /// Record, once, that the cleared-infra requeue found `scanned`'s
    /// publication does not authenticate, so later ticks leave it to an
    /// operator.
    fn refuse_transient_infra_requeue(
        &self,
        scanned: &TransientInfraBlockedTask,
        reason: &str,
    ) -> Result<(), OrbitError> {
        self.record_transient_infra_attempt(scanned, TRANSIENT_INFRA_REQUEUE_REFUSED_EVENT, reason)
    }

    /// Append `event` (a refusal or a deferral) to `scanned`'s block, while
    /// it is still the block that was scanned and nothing holds it.
    fn record_transient_infra_attempt(
        &self,
        scanned: &TransientInfraBlockedTask,
        event: &'static str,
        reason: &str,
    ) -> Result<(), OrbitError> {
        tracing::warn!(
            target: "orbit.core.transient_infra",
            task_id = %scanned.task_id,
            event,
            reason,
            "a transient-infrastructure block's publication was not authenticated"
        );
        self.stores()
            .tasks()
            .with_task_write_lock(&scanned.task_id, &mut || {
                let Some(current) = self.transient_infra_block_of(&scanned.task_id)? else {
                    return Ok(());
                };
                if current.blocked_at != scanned.blocked_at || current.held.is_some() {
                    return Ok(());
                }
                self.with_mutation(|| {
                    let updated = self.stores().task_records().update(
                        &scanned.task_id,
                        StoreTaskUpdateParams {
                            actor: SYSTEM_ACTOR_LABEL.to_string(),
                            append_history: vec![TaskHistoryEntry {
                                at: Utc::now(),
                                by: SYSTEM_ACTOR_LABEL.to_string(),
                                event: event.to_string(),
                                note: Some(format!("run_id={}: {reason}", scanned.run_id)),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
                                decision: None,
                            }],
                            expected_status: Some(vec![TaskStatus::Blocked]),
                            ..Default::default()
                        },
                    )?;
                    Ok((
                        updated,
                        OrbitEvent::TaskUpdated {
                            id: scanned.task_id.clone(),
                        },
                    ))
                })?;
                Ok(())
            })
    }
}

/// Nobody but Orbit's automation moved the task since the block, and it
/// never left `blocked`.
fn untouched_since(after: &[TaskHistoryEntry]) -> bool {
    after.iter().all(|entry| {
        entry.to_status.is_none()
            || (entry.by == SYSTEM_ACTOR_LABEL && entry.to_status == Some(TaskStatus::Blocked))
    })
}

/// The outcome name of the latest typed final-recovery decision Orbit
/// recorded for `run_id`.
fn latest_final_recovery<'a>(history: &'a [TaskHistoryEntry], run_id: &str) -> Option<&'a str> {
    history
        .iter()
        .rev()
        .find_map(|entry| match &entry.decision {
            Some(TaskDecision::FinalRecovery {
                run_id: recorded,
                outcome,
                ..
            }) if entry.by == SYSTEM_ACTOR_LABEL && recorded == run_id => Some(outcome.as_str()),
            _ => None,
        })
}
