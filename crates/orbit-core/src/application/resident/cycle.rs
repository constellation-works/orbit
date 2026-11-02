//! Trusted observe/finalize integration. The assignment lock fences a cycle
//! change against a comment append; no agent-facing tool can set this state.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::ResidentLiveCycle;

use crate::OrbitRuntime;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};

const CYCLE_CHANGED: &str = "resident_cycle_changed";

pub(super) fn live_cycle_from_history(
    history: &[TaskHistoryEntry],
) -> Result<Option<ResidentLiveCycle>, OrbitError> {
    let Some(entry) = history
        .iter()
        .rev()
        .find(|entry| entry.event == CYCLE_CHANGED)
    else {
        return Ok(None);
    };
    serde_json::from_str(entry.note.as_deref().unwrap_or_default()).map_err(|error| {
        OrbitError::Execution(format!("resident cycle state is unreadable: {error}"))
    })
}

impl OrbitRuntime {
    /// Publish or clear the live cycle under the assignment's write lock.
    /// Observe calls this after acquiring its lease; finalize clears with its
    /// own cycle as `expected_cycle`, so a dead finalizer cannot clear a newer
    /// cycle. This trusted runtime entry point has no tool/CLI write surface.
    pub fn set_resident_live_cycle(
        &self,
        task_id: &str,
        expected_cycle: Option<&str>,
        next: Option<&ResidentLiveCycle>,
    ) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if let Some(next) = next
            && (next.cycle.trim().is_empty()
                || next.run_id.trim().is_empty()
                || next.expires_at <= Utc::now())
        {
            return Err(OrbitError::InvalidInput(
                "live cycle needs a cycle id, run id and future lease expiry".into(),
            ));
        }
        self.stores()
            .tasks()
            .with_task_write_lock(task_id, &mut || {
                let task = self.get_task(task_id)?;
                self.ensure_resident_comment_assignment(&task)?;
                let history = self.get_task_history(task_id)?;
                let current = live_cycle_from_history(&history)?;
                if current.as_ref().map(|cycle| cycle.cycle.as_str()) != expected_cycle {
                    return Err(OrbitError::PolicyDenied(
                        "resident_cycle_stale: the live cycle changed".into(),
                    ));
                }
                if let Some(next) = next
                    && history
                        .iter()
                        .filter(|entry| entry.event == CYCLE_CHANGED)
                        .any(|entry| {
                            serde_json::from_str::<Option<ResidentLiveCycle>>(
                                entry.note.as_deref().unwrap_or_default(),
                            )
                            .ok()
                            .flatten()
                            .is_some_and(|old| old.cycle == next.cycle)
                        })
                {
                    return Err(OrbitError::InvalidInput(
                        "resident cycle ids cannot be reused".into(),
                    ));
                }
                let note = serde_json::to_string(&next).map_err(|error| {
                    OrbitError::Execution(format!("serialize live cycle: {error}"))
                })?;
                self.stores().task_records().update(
                    task_id,
                    TaskRecordUpdateParams {
                        actor: "system".into(),
                        append_history: vec![TaskHistoryEntry {
                            at: Utc::now(),
                            by: "system".into(),
                            event: CYCLE_CHANGED.into(),
                            note: Some(note),
                            from_status: None,
                            to_status: None,
                            operation_id: None,
                            decision: None,
                        }],
                        ..Default::default()
                    },
                )?;
                Ok(())
            })
    }
}
