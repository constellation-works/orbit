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
//! Once a probe shows the condition cleared — an immediate write transaction
//! on the task store, then the task's own write lock — the cleared-infra
//! requeue (`orbit task recheck-blocked --confirm`, and the owner's clock
//! tick) returns the task to the backlog through the same records an
//! operator requeue writes, so the next run resumes the candidate its failure
//! handoff published. A publication that no longer authenticates leaves the
//! task blocked for an operator.
//!
//! Each publication (or, unpublished, each task) takes at most
//! [`MAX_TRANSIENT_INFRA_REQUEUES`] such requeues; after that the block stays
//! for an operator, so a condition that keeps recurring cannot loop.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{TaskDecision, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::{
    MAX_TRANSIENT_INFRA_REQUEUES, TRANSIENT_INFRA_BLOCKED_EVENT, TRANSIENT_INFRA_REQUEUED_EVENT,
    TransientInfraBlock, TransientInfraRequeue, TransientInfrastructure,
};
use rusqlite::TransactionBehavior;

use super::helpers::SYSTEM_ACTOR_LABEL;
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
        }))
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

    /// Return every transient-infra blocked task whose probe passes and whose
    /// bound is not spent to the backlog, onto its published candidate. A
    /// task whose lock is still held, whose publication no longer
    /// authenticates, or that changed since the scan is left blocked.
    ///
    /// Stands down off the coordination owner, like every task write.
    pub fn requeue_cleared_transient_infra_blocks(
        &self,
    ) -> Result<Vec<TransientInfraBlockedTask>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut requeued = Vec::new();
        for scanned in self.transient_infra_blocked_tasks()? {
            if !scanned.cleared || scanned.exhausted {
                continue;
            }
            let mut cleared = None;
            let attempt = self
                .stores()
                .tasks()
                .with_task_write_lock(&scanned.task_id, &mut || {
                    let Some(current) = self.transient_infra_block_of(&scanned.task_id)? else {
                        return Ok(());
                    };
                    if current.blocked_at != scanned.blocked_at
                        || !current.cleared
                        || current.exhausted
                    {
                        return Ok(());
                    }
                    let task = self.get_task(&current.task_id)?;
                    let preserved = match &current.pull_request {
                        Some(_) => Some(self.failure_handoff_requeue_record(&task)?.0),
                        // A publication without a handoff receipt has nothing
                        // a requeue can authenticate; the operator decides.
                        None if self.run_published_through_pipeline(&current.run_id)? => {
                            return Ok(());
                        }
                        None => None,
                    };
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
                            preserved,
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
                });
            if let Err(error) = attempt {
                tracing::warn!(
                    target: "orbit.core.transient_infra",
                    task_id = %scanned.task_id,
                    error = %error,
                    "left a transient-infrastructure block in place"
                );
            }
            requeued.extend(cleared);
        }
        Ok(requeued)
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
