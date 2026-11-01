//! The deterministic recheck: settle a blocked task with no agent when the
//! world already fixed what blocked it, and record why.
//!
//! One pass reads every blocked task and settles one that meets any of:
//!
//! - **external merge**: its delivery pull request merged outside Orbit, so
//!   the task follows the merge to `done` (the settlement `orbit task
//!   settle-merged` makes);
//! - **checks green**: the failing required checks were recorded as
//!   inherited from the base (`pr_auto_fix_base_red`, `ci_base_suspect`) and
//!   the pull request's required checks all pass now, so the task returns to
//!   `backlog` and its next run continues the publication. Counted in the
//!   shared requeue bound; an operator-only task is left alone;
//! - **infrastructure cleared**: the block was a missing provider launcher or
//!   transient infrastructure whose probe now passes (the requeue `orbit task
//!   recheck-blocked --confirm` makes).
//!
//! Every settlement appends a [`BLOCKED_TASK_RECHECKED_EVENT`] history entry
//! naming the kind and why. A task that settles leaves `blocked`, so its open
//! decision request reads superseded. The pass makes no judgment call: a task
//! it cannot settle is left for final recovery.

use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{TaskComment, TaskHistoryEntry, TaskStatus};
use serde::Serialize;

use super::super::TaskRecordUpdateParams as StoreTaskUpdateParams;
use super::super::final_recovery::{
    FINAL_RECOVERY_REQUEUED_EVENT, operator_only_requeue, records_base_red,
};
use super::super::helpers::SYSTEM_ACTOR_LABEL;
use crate::OrbitRuntime;
use crate::application::task::TaskRecords;

/// History event a recheck settlement appends, naming its kind and why.
pub const BLOCKED_TASK_RECHECKED_EVENT: &str = "blocked_task_rechecked";

/// Why a blocked task was settled without an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecheckKind {
    /// Its delivery pull request merged outside Orbit.
    ExternalMerge,
    /// Its failing required checks pass now.
    ChecksGreen,
    /// Its infrastructure block cleared.
    InfraCleared,
}

impl RecheckKind {
    /// Stable token for the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalMerge => "external_merge",
            Self::ChecksGreen => "checks_green",
            Self::InfraCleared => "infra_cleared",
        }
    }
}

/// One blocked task the recheck settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BlockedRecheckSettlement {
    /// The settled task.
    pub task_id: String,
    /// What settled it.
    pub kind: RecheckKind,
    /// The evidence, as the history entry records it.
    pub why: String,
}

impl OrbitRuntime {
    /// Settle every blocked task whose block cleared by itself, without an
    /// agent. Stands down off the coordination owner, like every task write.
    pub fn recheck_blocked_tasks(&self) -> Result<Vec<BlockedRecheckSettlement>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut settled = Vec::new();
        let blocked =
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?;
        for task in blocked {
            match self.settle_external_merge_of(&task.id) {
                Ok(Some(settlement)) => {
                    settled.push(settlement);
                    continue;
                }
                Ok(None) => {}
                Err(error) => warn_left_blocked(&task.id, "external merge", &error),
            }
            match self.settle_green_checks_of(&task.id) {
                Ok(Some(settlement)) => settled.push(settlement),
                Ok(None) => {}
                Err(error) => warn_left_blocked(&task.id, "checks green", &error),
            }
        }
        // Infrastructure blocks: the requeues write their own cleared note;
        // the recheck adds its uniform entry.
        let mut cleared = Vec::new();
        match self.requeue_cleared_infra_blocked_tasks() {
            Ok(tasks) => cleared.extend(tasks.into_iter().map(|task| {
                let why = format!(
                    "the provider launcher `{}` for `{}` resolves again",
                    task.program, task.provider
                );
                (task.task_id, why)
            })),
            Err(error) => warn_left_blocked("*", "infrastructure", &error),
        }
        match self.requeue_cleared_transient_infra_blocks() {
            Ok(tasks) => cleared.extend(tasks.into_iter().map(|task| {
                let why = format!(
                    "transient infrastructure ({}) cleared: its probe passes",
                    task.class.as_str()
                );
                (task.task_id, why)
            })),
            Err(error) => warn_left_blocked("*", "transient infrastructure", &error),
        }
        for (task_id, why) in cleared {
            self.record_recheck(&task_id, RecheckKind::InfraCleared, &why, None)?;
            settled.push(BlockedRecheckSettlement {
                task_id,
                kind: RecheckKind::InfraCleared,
                why,
            });
        }
        Ok(settled)
    }

    fn settle_external_merge_of(
        &self,
        task_id: &str,
    ) -> Result<Option<BlockedRecheckSettlement>, OrbitError> {
        // A task that cannot settle is refused with its reason; that is the
        // common case and says nothing about the block.
        let Ok(settlement) = self.settle_externally_merged_task(task_id) else {
            return Ok(None);
        };
        let why = format!(
            "its delivery pull request #{} merged into {} as {} outside Orbit",
            settlement.pr_number, settlement.base_branch, settlement.merge_commit
        );
        self.record_recheck(task_id, RecheckKind::ExternalMerge, &why, None)?;
        Ok(Some(BlockedRecheckSettlement {
            task_id: task_id.to_string(),
            kind: RecheckKind::ExternalMerge,
            why,
        }))
    }

    fn settle_green_checks_of(
        &self,
        task_id: &str,
    ) -> Result<Option<BlockedRecheckSettlement>, OrbitError> {
        let task = self.get_task(task_id)?;
        let Some(number) = task
            .github_pr_number()
            .and_then(|number| number.parse::<u64>().ok())
        else {
            return Ok(None);
        };
        let history = self.get_task_history(task_id)?;
        if !records_base_red(&history) || operator_only_requeue(self, &task).is_some() {
            return Ok(None);
        }
        if self.mechanical_bound(&task, "recheck").is_err() {
            return Ok(None);
        }
        let forge = self.task_pr_forge();
        let Some(blockers) = forge.pull_request_blockers(&self.paths().repo_root, number)? else {
            return Ok(None);
        };
        if blockers.conflicting
            || !blockers.failed_required_checks.is_empty()
            || !blockers.pending_required_checks.is_empty()
            || blockers.passed_required_checks.is_empty()
        {
            return Ok(None);
        }
        let why = format!(
            "the required checks that failed as inherited from the base ({}) pass on head {} of \
             pull request #{number}",
            blockers.passed_required_checks.join(", "),
            blockers.head_sha
        );
        let mut settled = false;
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let current = self.get_task(task_id)?;
                if current.status != TaskStatus::Blocked || current.updated_at != task.updated_at {
                    return Ok(());
                }
                let now = chrono::Utc::now();
                self.with_mutation(|| {
                    let updated = self.stores().task_records().update(
                        task_id,
                        StoreTaskUpdateParams {
                            actor: SYSTEM_ACTOR_LABEL.to_string(),
                            status: Some(TaskStatus::Backlog),
                            status_event: Some(FINAL_RECOVERY_REQUEUED_EVENT.to_string()),
                            status_note: Some(format!("blocked-task recheck: {why}")),
                            append_comments: vec![TaskComment {
                                at: now,
                                by: SYSTEM_ACTOR_LABEL.to_string(),
                                message: format!(
                                    "blocked_task_recheck kind=checks_green\nwhy: {why}\nThe task \
                                 returns to backlog; its next run continues the publication."
                                ),
                                operation_id: None,
                            }],
                            append_history: vec![recheck_entry(RecheckKind::ChecksGreen, &why)],
                            expected_status: Some(vec![TaskStatus::Blocked]),
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
                settled = true;
                Ok(())
            })?;
        Ok(settled.then(|| BlockedRecheckSettlement {
            task_id: task_id.to_string(),
            kind: RecheckKind::ChecksGreen,
            why,
        }))
    }

    /// Append the recheck entry (and nothing else) to a task a settlement
    /// already moved.
    fn record_recheck(
        &self,
        task_id: &str,
        kind: RecheckKind,
        why: &str,
        comment: Option<TaskComment>,
    ) -> Result<(), OrbitError> {
        self.with_mutation(|| {
            let updated = self.stores().task_records().update(
                task_id,
                StoreTaskUpdateParams {
                    actor: SYSTEM_ACTOR_LABEL.to_string(),
                    append_comments: comment.into_iter().collect(),
                    append_history: vec![recheck_entry(kind, why)],
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
    }
}

fn recheck_entry(kind: RecheckKind, why: &str) -> TaskHistoryEntry {
    TaskHistoryEntry {
        at: chrono::Utc::now(),
        by: SYSTEM_ACTOR_LABEL.to_string(),
        event: BLOCKED_TASK_RECHECKED_EVENT.to_string(),
        note: Some(format!("kind={} why={why}", kind.as_str())),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    }
}

fn warn_left_blocked(task_id: &str, check: &str, error: &OrbitError) {
    tracing::warn!(
        target: "orbit.core.blocked_recovery",
        task_id = %task_id,
        "the {check} recheck left a blocked task in place: {error}"
    );
}
