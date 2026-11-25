use chrono::{DateTime, Duration, Local, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{RoutineFireIntentParams, RoutineFireState};

use super::super::due::{MISSED_SLOT_SKIP_DETAIL, MissedSlotWalk, missed_slots_through};
use super::super::loader::LoadedRoutine;
use super::contract::RoutineSweepReport;
use super::report::skipped;

/// How many missed `skip` slots one tick records. Due math stays O(1); the
/// cursor must not jump past a slot this pass did not record, so a longer
/// gap continues on the next tick before a newer natural slot can fire.
const MISSED_SLOT_RECORD_LIMIT: usize = 64;

/// Bounds for one `missed_run: skip` recording pass.
pub(super) struct SkipWindow<'a> {
    pub(super) cron: &'a croner::Cron,
    pub(super) lower_bound: &'a DateTime<Local>,
    pub(super) through: &'a DateTime<Local>,
    pub(super) now_local: &'a DateTime<Local>,
    pub(super) natural_slot_grace: Duration,
    pub(super) include_through: bool,
}

/// Record past-grace slots in `(lower_bound, through]`, oldest first.
pub(super) fn record_missed_slots(
    store: &orbit_store::Store,
    routine: &LoadedRoutine,
    window: &SkipWindow<'_>,
    dry_run: bool,
) -> Result<MissedSlotWalk<Local>, OrbitError> {
    let walk = missed_slots_through(
        window.cron,
        window.lower_bound,
        window.through,
        window.now_local,
        window.natural_slot_grace,
        window.include_through,
        MISSED_SLOT_RECORD_LIMIT,
    )?;
    if dry_run {
        return Ok(walk);
    }
    for slot in &walk.slots {
        let slot_utc = slot.with_timezone(&Utc).to_rfc3339();
        let claimed = super::fire::record_fire_intent(
            store,
            &RoutineFireIntentParams {
                routine_name: routine.definition.name.clone(),
                slot: slot_utc.clone(),
                attempt: 1,
                source_workspace: routine.source_workspace.clone(),
            },
            true,
        )?;
        if !claimed {
            continue;
        }
        store.write(|tx| {
            tx.routine().update_fire(
                (&routine.definition.name, &slot_utc, 1),
                RoutineFireState::Skipped,
                None,
                Some(MISSED_SLOT_SKIP_DETAIL),
            )
        })?;
    }
    Ok(walk)
}

pub(super) fn missed_skip_report(
    routine: &LoadedRoutine,
    walk: &MissedSlotWalk<Local>,
) -> RoutineSweepReport {
    let slot = walk
        .slots
        .last()
        .map(|slot| slot.with_timezone(&Utc).to_rfc3339());
    RoutineSweepReport {
        slot,
        ..skipped(routine, MISSED_SLOT_SKIP_DETAIL)
    }
}
