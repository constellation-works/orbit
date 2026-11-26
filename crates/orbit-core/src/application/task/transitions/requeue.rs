use std::path::PathBuf;

use chrono::{DateTime, Utc};
use orbit_engine::WORKFLOW_RUN_FAILED_EVENT;
use orbit_engine::activity_job::cli_runner::missing_launcher_in;
use orbit_types::task::TaskHistoryEntry;

use super::*;

/// Status event recorded when `orbit task recheck-blocked --confirm` returns a
/// task blocked by a missing provider launcher to backlog because the launcher
/// now resolves.
pub(crate) const INFRA_BLOCK_CLEARED_EVENT: &str = "infra_block_cleared";

/// A blocked task whose block was caused by host configuration — its run
/// failed because dispatch could not find the provider launcher — rather than
/// by the task's own work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InfraBlockedTask {
    pub task_id: String,
    pub title: String,
    /// When the blocking history entry was recorded; identifies the block, so
    /// a requeue can confirm it is still the one it classified.
    pub blocked_at: DateTime<Utc>,
    pub run_id: Option<String>,
    /// Launcher program named by the failure, as the executor configured it.
    pub program: String,
    pub provider: String,
    /// Where dispatch from this workspace would launch `program` now, or
    /// `None` while the condition still reproduces.
    pub launcher: Option<PathBuf>,
}

impl OrbitRuntime {
    /// Blocked tasks in this workspace whose block is a missing provider
    /// launcher, each with where that launcher resolves now. Resolution uses
    /// dispatch's own lookup, from this process's `PATH` and `HOME`.
    pub fn infra_blocked_tasks(&self) -> Result<Vec<InfraBlockedTask>, OrbitError> {
        let blocked =
            self.list_tasks_filtered(Some(TaskStatus::Blocked), None, None, None, None, None)?;
        let mut infra_blocked = Vec::new();
        for task in blocked {
            if let Some(entry) = self.infra_block_of(&task)? {
                infra_blocked.push(entry);
            }
        }
        Ok(infra_blocked)
    }

    /// Classify one task's current block; `None` unless it is blocked by a
    /// missing provider launcher.
    pub fn infra_block_of(&self, task: &Task) -> Result<Option<InfraBlockedTask>, OrbitError> {
        if task.status != TaskStatus::Blocked {
            return Ok(None);
        }
        let history = self.get_task_history(&task.id)?;
        let Some((entry, program, provider)) = launcher_block(&history) else {
            return Ok(None);
        };
        let launcher = self.locate_provider_launcher(&program);
        Ok(Some(InfraBlockedTask {
            task_id: task.id.clone(),
            title: task.title.clone(),
            blocked_at: entry.at,
            run_id: task.job_run_id.clone(),
            program,
            provider,
            launcher,
        }))
    }

    /// Return every task blocked by a missing provider launcher that now
    /// resolves to `backlog`, recording which launcher cleared which block.
    /// Tasks whose launcher is still missing, and every task blocked for any
    /// other reason, are left alone.
    ///
    /// Each task is re-classified under its write lock and written with a
    /// `blocked` compare-and-set, so an operator decision or a newer block
    /// that lands after the scan wins over this requeue.
    pub fn requeue_cleared_infra_blocked_tasks(&self) -> Result<Vec<InfraBlockedTask>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let mut requeued = Vec::new();
        for scanned in self.infra_blocked_tasks()? {
            if scanned.launcher.is_none() {
                continue;
            }
            let mut cleared = None;
            self.stores()
                .tasks()
                .with_task_write_lock(&scanned.task_id, &mut || {
                    let task = self.get_task(&scanned.task_id)?;
                    let Some(current) = self.infra_block_of(&task)? else {
                        return Ok(());
                    };
                    let Some(launcher) = current.launcher.as_ref() else {
                        return Ok(());
                    };
                    if current.blocked_at != scanned.blocked_at {
                        return Ok(());
                    }
                    let note = format!(
                        "infra block cleared by `orbit task recheck-blocked`: provider launcher \
                         `{}` for provider `{}` now resolves at {}; the block recorded at {} \
                         (run_id={}) no longer reproduces",
                        current.program,
                        current.provider,
                        launcher.display(),
                        current.blocked_at.to_rfc3339(),
                        current.run_id.as_deref().unwrap_or("-"),
                    );
                    self.stores().task_records().update(
                        &current.task_id,
                        StoreTaskUpdateParams {
                            actor: SYSTEM_ACTOR_LABEL.to_string(),
                            status_event: Some(INFRA_BLOCK_CLEARED_EVENT.to_string()),
                            status_note: Some(note.clone()),
                            expected_status: Some(vec![TaskStatus::Blocked]),
                            ..StoreTaskUpdateParams::from(TaskUpdateParams {
                                status: Some(TaskStatus::Backlog),
                                ..Default::default()
                            })
                        },
                    )?;
                    cleared = Some(current);
                    Ok(())
                })?;
            requeued.extend(cleared);
        }
        Ok(requeued)
    }
}

/// The entry that put the task in its current block, when that entry is a
/// workflow failure naming a missing provider launcher. A later block of any
/// other kind (an operator's, another run's failure) supersedes it.
fn launcher_block(history: &[TaskHistoryEntry]) -> Option<(&TaskHistoryEntry, String, String)> {
    let entry = history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Blocked))?;
    if entry.event != WORKFLOW_RUN_FAILED_EVENT {
        return None;
    }
    let missing = missing_launcher_in(entry.note.as_deref()?)?;
    Some((entry, missing.program, missing.provider))
}
