//! The `Scheduled` admission hold: a `backlog` task waits for its
//! `not_before`, and the hold lifts on the first admission pass after it.
//!
//! Every dispatcher reads the hold through [`OrbitRuntime::schedule_hold`] or
//! [`schedule_hold_in`]: the auto drain's backlog snapshot, the owner's pull
//! admission, and the workflow-entry check the explicit ship, the gate's
//! dispatch re-check and worktree admission share. An operator's CLI
//! `--ignore-schedule` records a `schedule_overridden` history event for the
//! scheduled time, which every one of them honours.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::{
    SCHEDULE_OVERRIDDEN_EVENT, Task, TaskHistoryEntry, TaskStatus, format_not_before,
    schedule_overridden, schedule_override_note, scheduled_wait,
};

use super::{TaskRecordUpdateParams, TaskRecords};
use crate::OrbitRuntime;

/// A `backlog` task still waiting for its scheduled start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScheduleHold {
    pub(crate) not_before: DateTime<Utc>,
}

impl ScheduleHold {
    /// Why the task waits, naming the time.
    pub(crate) fn detail(self) -> String {
        scheduled_wait(self.not_before)
    }

    /// The refusal an explicit dispatch of `task_id` gets.
    pub(crate) fn refusal(self, task_id: &str) -> OrbitError {
        OrbitError::TaskScheduled {
            task_id: task_id.to_string(),
            not_before: format_not_before(self.not_before),
        }
    }
}

/// The hold on `task` at `now`: a `backlog` task whose `not_before` is still
/// ahead and that no operator override names. History is read only for a
/// task that is actually scheduled ahead.
pub(crate) fn schedule_hold_in(
    task: &Task,
    now: DateTime<Utc>,
    history: &dyn Fn() -> Result<Vec<TaskHistoryEntry>, OrbitError>,
) -> Result<Option<ScheduleHold>, OrbitError> {
    if task.status != TaskStatus::Backlog {
        return Ok(None);
    }
    let Some(not_before) = task.scheduled_after(now) else {
        return Ok(None);
    };
    if schedule_overridden(&history()?, not_before) {
        return Ok(None);
    }
    Ok(Some(ScheduleHold { not_before }))
}

impl OrbitRuntime {
    /// The scheduled hold on `task` now; see [`schedule_hold_in`].
    pub(crate) fn schedule_hold(&self, task: &Task) -> Result<Option<ScheduleHold>, OrbitError> {
        schedule_hold_in(task, Utc::now(), &|| self.get_task_history(&task.id))
    }

    /// Record an operator's early start of `task_id`, scheduled for
    /// `not_before`. Written before the run is submitted, because its worker
    /// starts at once; the event lifts the hold for that time on every
    /// admission route.
    pub(crate) fn record_schedule_override(
        &self,
        task_id: &str,
        not_before: DateTime<Utc>,
        actor: &str,
    ) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().task_records().update(
            task_id,
            TaskRecordUpdateParams {
                actor: actor.to_string(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: actor.to_string(),
                    event: SCHEDULE_OVERRIDDEN_EVENT.to_string(),
                    note: Some(schedule_override_note(not_before, actor)),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                }],
                ..Default::default()
            },
        )?;
        Ok(())
    }
}
