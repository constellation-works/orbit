use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{RESIDENT_CYCLE_CHANGED_EVENT, ResidentCycleChange, ResidentLiveCycle};

#[cfg(feature = "test-support")]
use super::test_support;
use crate::OrbitRuntime;
use crate::app::resident::ResidentMandateState;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};

/// Publish or clear the live cycle under the assignment's write lock.
/// Observe calls this after acquiring its lease; finalize clears with its
/// own cycle as `expected_cycle`, so a dead finalizer cannot clear a newer
/// cycle. Publication rechecks the mandate's active state under the same
/// lock as manager control; clearing an admitted cycle remains allowed
/// after pause, takeover or stop. This trusted runtime entry point has no
/// tool/CLI write surface.
pub fn set_resident_live_cycle(
    rt: &OrbitRuntime,
    task_id: &str,
    expected_cycle: Option<&str>,
    next: Option<&ResidentLiveCycle>,
) -> Result<(), OrbitError> {
    set_resident_live_cycle_inner(rt, task_id, expected_cycle, next, false).map(|_| ())
}

/// Publish the live cycle like [`set_resident_live_cycle`], but
/// also return the assignment history as read under the publication
/// lock, before the new cycle entry is appended. A manager control
/// write committed in that snapshot raced with the caller's earlier
/// observation, so the caller must re-derive any wake reasons from it;
/// a write committed after the lock is released postdates the new
/// cycle's start and stays pending for the next cycle.
pub(crate) fn publish_resident_live_cycle(
    rt: &OrbitRuntime,
    task_id: &str,
    expected_cycle: Option<&str>,
    next: &ResidentLiveCycle,
) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
    set_resident_live_cycle_inner(rt, task_id, expected_cycle, Some(next), true)
        .map(|history| history.unwrap_or_default())
}

fn set_resident_live_cycle_inner(
    rt: &OrbitRuntime,
    task_id: &str,
    expected_cycle: Option<&str>,
    next: Option<&ResidentLiveCycle>,
    capture_prior_history: bool,
) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
    rt.ensure_coordination_task_write_permitted()?;
    if let Some(next) = next
        && (next.cycle.trim().is_empty()
            || next.run_id.trim().is_empty()
            || next.expires_at <= Utc::now())
    {
        return Err(OrbitError::InvalidInput(
            "live cycle needs a cycle id, run id and future lease expiry".into(),
        ));
    }
    #[cfg(feature = "test-support")]
    if next.is_some() {
        test_support::before_resident_cycle_publication();
    }
    let change = next
        .cloned()
        .map_or(ResidentCycleChange::Cleared, ResidentCycleChange::Started);
    let mut prior_history = None;
    rt.stores().tasks().with_task_write_lock(task_id, &mut || {
        let task = rt.get_task(task_id)?;
        crate::app::resident::comment::ensure_resident_comment_assignment(rt, &task)?;
        let history = rt.get_task_history(task_id)?;
        if capture_prior_history {
            prior_history = Some(history.clone());
        }
        if next.is_some()
            && crate::app::resident::read::resident_view(rt, task, &history)?
                .state()
                .0
                != ResidentMandateState::Active
        {
            return Err(OrbitError::PolicyDenied(
                "resident_cycle_not_active: the mandate no longer admits new cycles".into(),
            ));
        }
        let current = live_cycle_from_history(&history)?;
        if current.as_ref().map(|cycle| cycle.cycle.as_str()) != expected_cycle {
            return Err(OrbitError::PolicyDenied(
                "resident_cycle_stale: the live cycle changed".into(),
            ));
        }
        if let Some(next) = next
            && history.iter().any(|entry| {
                matches!(
                    ResidentCycleChange::from_entry(entry),
                    Some(Ok(ResidentCycleChange::Started(old))) if old.cycle == next.cycle
                )
            })
        {
            return Err(OrbitError::InvalidInput(
                "resident cycle ids cannot be reused".into(),
            ));
        }
        let note = change
            .to_note()
            .map_err(|error| OrbitError::Execution(format!("serialize live cycle: {error}")))?;
        rt.stores().task_records().update(
            task_id,
            TaskRecordUpdateParams {
                actor: "system".into(),
                append_history: vec![TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: RESIDENT_CYCLE_CHANGED_EVENT.into(),
                    note: Some(note),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                    surface: None,
                    caller: None,
                }],
                ..Default::default()
            },
        )?;
        Ok(())
    })?;
    Ok(prior_history)
}

/// The live cycle the latest cycle change records. A malformed latest change
/// refuses the read: an unreadable lease is never treated as clear.
pub(in crate::app::resident) fn live_cycle_from_history(
    history: &[TaskHistoryEntry],
) -> Result<Option<ResidentLiveCycle>, OrbitError> {
    match history
        .iter()
        .rev()
        .find_map(ResidentCycleChange::from_entry)
    {
        None | Some(Ok(ResidentCycleChange::Cleared)) => Ok(None),
        Some(Ok(ResidentCycleChange::Started(live))) => Ok(Some(live)),
        Some(Err(error)) => Err(OrbitError::Execution(format!(
            "resident cycle state is unreadable: {error}"
        ))),
    }
}
