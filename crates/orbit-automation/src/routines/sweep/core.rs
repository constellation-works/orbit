use chrono::{DateTime, Local, SecondsFormat, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::RoutineStoreBackend;
use orbit_types::workflow::automation::ConditionGuardMiss;
use orbit_types::workflow::{MissedRunPolicy, OverlapPolicy};
use std::collections::BTreeMap;
use std::time::Instant;

use super::super::breaker::{BreakerGate, breaker_gate};
use super::super::due::{
    DueDecision, due_decision_with_grace, natural_slot_grace_for_cadence, parse_cron,
};
use super::super::loader::{LoadedRoutine, RoutineCollection};
use super::TICK_DEADLINE;
use super::contract::{RoutineDispatch, RoutineSweepReport, SweepOptions};
use super::fire::{FireRequest, fire, sync_unresolved_fires};
use super::missed_slots::{SkipWindow, missed_skip_report, record_missed_slots};
use super::report::{action, failure_report, parse_rfc3339, skipped};
use super::retry::{next_retry_attempt, retry_candidate, retryable_failure};

/// The dispatch-agnostic core of one sweep pass: outcome-sync
/// (unless dry-run), then per-routine due evaluation and fire/skip. Split out
/// from Core assembly — which owns the lock, store, and workspace discovery
/// — so the orchestration can be driven against a temp store, a hand-built
/// [`RoutineCollection`], a fake [`RoutineDispatch`], and an explicit `now`.
pub fn run_sweep_core(
    store: &dyn RoutineStoreBackend,
    collection: &RoutineCollection,
    dispatch: &dyn RoutineDispatch,
    options: SweepOptions,
    now_utc: DateTime<Utc>,
) -> Result<Vec<RoutineSweepReport>, OrbitError> {
    let deadline = options
        .deadline
        .unwrap_or_else(|| Instant::now() + TICK_DEADLINE);
    let routines_by_name: BTreeMap<String, &LoadedRoutine> = collection
        .routines
        .iter()
        .map(|routine| (routine.definition.name.clone(), routine))
        .collect();

    let sync_errors = if options.dry_run {
        BTreeMap::new()
    } else {
        sync_unresolved_fires(store, &routines_by_name, dispatch, now_utc, deadline)?
    };

    let pauses = store.routine_pauses()?;
    let breakers = store.routine_breakers()?;

    let mut reports = Vec::new();
    for routine in &collection.routines {
        if Instant::now() >= deadline {
            reports.push(skipped(routine, "tick_deadline"));
            continue;
        }
        if duplicate_ownership_skipped(&collection.routines, routine) {
            reports.push(skipped(routine, "duplicate_routine_ownership"));
            continue;
        }
        if let Some(reason) = sync_errors.get(&routine.definition.name) {
            reports.push(failure_report(routine, reason.clone()));
            continue;
        }
        // [ORB-25103] An open circuit breaker holds every trigger kind,
        // before any evaluation or retry, except for its one trial pass.
        let gate = breaker_gate(
            store,
            &breakers,
            routine,
            dispatch,
            options.dry_run,
            now_utc,
        );
        let report = match gate {
            Ok(BreakerGate::Skip(reason)) => skipped(routine, &reason),
            Ok(BreakerGate::Proceed) => {
                sweep_routine(store, routine, dispatch, &pauses, options, now_utc)
                    .unwrap_or_else(|error| failure_report(routine, error.to_string()))
            }
            Ok(BreakerGate::BypassFire) => {
                bypass_fire(store, routine, dispatch, &pauses, options, now_utc)
                    .unwrap_or_else(|error| failure_report(routine, error.to_string()))
            }
            Err(error) => failure_report(routine, error.to_string()),
        };
        reports.push(report);
    }

    Ok(reports)
}

/// Whether the sweep skips `routine` as a duplicate owner of its job: an
/// enabled peer from the same source `.orbit` dir and target conflicts with it.
/// A disabled routine owns nothing, so it is never skipped this way. Status
/// reporting calls this too, so `routine show` names the skip the sweep records.
pub fn duplicate_ownership_skipped(routines: &[LoadedRoutine], routine: &LoadedRoutine) -> bool {
    routine.definition.enabled
        && routines.iter().any(|other| {
            !std::ptr::eq(other, routine)
                && other.definition.enabled
                && other.source_orbit_dir == routine.source_orbit_dir
                && other.definition.target == routine.definition.target
                && ownership_conflicts(routine, other)
        })
}

/// Whether two routines on one target would both own its job. A temporal (cron)
/// routine and a state routine conflict: the migration guard [ORB-11331] keeps
/// a definition and its state replacement from both running. Two state routines
/// conflict only on the same trigger kind, since each kind selects its own
/// consumer and member set [ORB-17918]. Two routines with no state trigger never
/// conflict.
pub(super) fn ownership_conflicts(left: &LoadedRoutine, right: &LoadedRoutine) -> bool {
    match (
        &left.definition.trigger.state,
        &right.definition.trigger.state,
    ) {
        (Some(a), Some(b)) => a.kind == b.kind,
        (None, None) => false,
        _ => true,
    }
}

/// Report reason of a fire an operator's breaker bypass dispatched.
pub(super) const BREAKER_BYPASS_FIRE_REASON: &str = "breaker_bypass";

/// [ORB-25103] The trial an operator's breaker bypass admitted. A cron
/// routine fires once now, due slot or not, under an unscheduled slot key:
/// its cursor and scheduled slots stay untouched, so they fire or skip by
/// the usual rules once the breaker closes. Other trigger kinds wait for no
/// slot and are evaluated as usual.
pub(super) fn bypass_fire(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    pauses: &BTreeMap<String, orbit_store::contracts::RoutinePauseRecord>,
    options: SweepOptions,
    now_utc: DateTime<Utc>,
) -> Result<RoutineSweepReport, OrbitError> {
    let definition = &routine.definition;
    let trigger = &definition.trigger;
    if trigger.deliveries_landed.is_some() || trigger.state.is_some() || trigger.condition.is_some()
    {
        return sweep_routine(store, routine, dispatch, pauses, options, now_utc);
    }
    if !definition.enabled {
        return Ok(skipped(routine, "disabled_in_definition"));
    }
    if pauses.contains_key(&definition.name) {
        return Ok(skipped(routine, "paused_locally"));
    }
    // Nanosecond precision keeps the key distinct from every whole-minute
    // cron slot, even when the sweep runs exactly on one.
    let slot = now_utc.to_rfc3339_opts(SecondsFormat::Nanos, false);
    fire(
        store,
        routine,
        dispatch,
        FireRequest {
            slot: &slot,
            attempt: 1,
            fired_action: "fired",
            unscheduled: true,
            options,
            now_utc,
        },
    )
}

pub(super) fn sweep_routine(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    pauses: &BTreeMap<String, orbit_store::contracts::RoutinePauseRecord>,
    options: SweepOptions,
    now_utc: DateTime<Utc>,
) -> Result<RoutineSweepReport, OrbitError> {
    let definition = &routine.definition;
    let name = &definition.name;
    if (definition.trigger.deliveries_landed.is_some() || definition.trigger.state.is_some())
        && !pauses.contains_key(name)
    {
        let diagnostic = dispatch.evaluate_delivery(routine, options.dry_run, now_utc)?;
        let run_id = diagnostic.state.as_ref().and_then(|s| {
            s.active
                .as_ref()
                .and_then(|a| a.action_id.clone())
                .or_else(|| {
                    s.members
                        .as_ref()
                        .and_then(|m| m.active.as_ref())
                        .and_then(|a| a.action_id.clone())
                })
        });
        return Ok(RoutineSweepReport {
            routine: name.clone(),
            source: routine.source_workspace.clone(),
            origin: routine.origin.as_str(),
            action: if definition.trigger.state.is_some() {
                "state"
            } else {
                "delivery"
            },
            reason: Some(diagnostic.reason),
            slot: None,
            run_id,
            batch: diagnostic.batch,
        });
    }

    // A condition routine is evaluated while disabled too: seeing the
    // disable makes the next enable a baseline. A host-local pause holds it
    // without touching its edge state.
    if definition.trigger.condition.is_some() {
        if pauses.contains_key(name) {
            return Ok(skipped(routine, "paused_locally"));
        }
        let diagnostic = dispatch.evaluate_condition(routine, options.dry_run, now_utc)?;
        let mut reason = diagnostic.reason.clone();
        if !diagnostic.would_fire.is_empty() {
            reason = format!("would_fire: {}", diagnostic.would_fire.join(", "));
        }
        return Ok(RoutineSweepReport {
            reason: Some(reason),
            run_id: diagnostic
                .actions
                .iter()
                .rev()
                .find_map(|action| action.action_id.clone()),
            ..action(routine, "condition")
        });
    }

    // Toggle resolution order (2_design.md §4): versioned kill-switch →
    // host-local pause.
    if !definition.enabled {
        return Ok(skipped(routine, "disabled_in_definition"));
    }
    if pauses.contains_key(name) {
        return Ok(skipped(routine, "paused_locally"));
    }

    let cron = parse_cron(&definition.trigger.cron)?;
    let now_local = now_utc.with_timezone(&Local);

    let Some(cursor) = store.routine_cursor(name)? else {
        // First observation on this host: record the baseline and fire
        // nothing — a routine never fires for slots that predate its
        // registration here.
        if options.dry_run {
            return Ok(action(routine, "would_baseline"));
        }
        store.routine_record_baseline(name, &now_utc.to_rfc3339())?;
        return Ok(action(routine, "baselined"));
    };

    let lower_bound_raw = cursor.last_slot.as_deref().unwrap_or(&cursor.baseline_at);
    let lower_bound = parse_rfc3339(lower_bound_raw)?.with_timezone(&Local);

    let natural_slot_grace = natural_slot_grace_for_cadence(options.sweep_cadence_seconds)?;
    let decision = due_decision_with_grace(
        &cron,
        definition.trigger.missed_run,
        &lower_bound,
        &now_local,
        natural_slot_grace,
    )?;
    // Skip rows are terminal and sort ahead of an older in-flight or failed
    // fire. Recording them while either is outstanding hides that fire from a
    // latest-row check, so do not write them until both are clear. A new due
    // fire still takes precedence over a retry and is handled below.
    let suppress_skips = store.routine_blocking_fire(name)?.is_some()
        || retryable_failure(store, routine)?.is_some();
    // A waiting `when:` guard keeps a due slot unconsumed past its natural
    // grace: it fires on the first later pass where the guard is true, and
    // the next slot collapses it (§11).
    let guard_waits = definition.trigger.when.is_some()
        && definition.trigger.guard_miss != Some(ConditionGuardMiss::Skip);
    let decision = match decision {
        DueDecision::RecordSkip { slot } if guard_waits && !suppress_skips => DueDecision::Fire {
            slot,
            is_catch_up: false,
        },
        other => other,
    };

    match decision {
        DueDecision::Fire { slot, is_catch_up } => {
            if definition.trigger.missed_run == MissedRunPolicy::Skip
                && !is_catch_up
                && !suppress_skips
            {
                let walk = record_missed_slots(
                    store,
                    routine,
                    &SkipWindow {
                        cron: &cron,
                        lower_bound: &lower_bound,
                        through: &slot,
                        now_local: &now_local,
                        natural_slot_grace,
                        include_through: false,
                    },
                    options.dry_run,
                )?;
                if walk.more {
                    return Ok(missed_skip_report(routine, &walk));
                }
            }
            let slot_utc = slot.with_timezone(&Utc).to_rfc3339();
            fire(
                store,
                routine,
                dispatch,
                FireRequest {
                    slot: &slot_utc,
                    attempt: 1,
                    fired_action: "fired",
                    unscheduled: false,
                    options,
                    now_utc,
                },
            )
        }
        DueDecision::RecordSkip { slot } => {
            if suppress_skips {
                if definition.policy.overlap == OverlapPolicy::Forbid
                    && store.routine_blocking_fire(name)?.is_some()
                {
                    return Ok(RoutineSweepReport {
                        slot: Some(slot.with_timezone(&Utc).to_rfc3339()),
                        ..skipped(routine, "overlap_in_flight")
                    });
                }
                if let Some(retry) = retry_candidate(store, routine, now_utc)? {
                    return fire(
                        store,
                        routine,
                        dispatch,
                        FireRequest {
                            slot: &retry.slot,
                            attempt: next_retry_attempt(store, &retry)?,
                            fired_action: "retry_fired",
                            unscheduled: false,
                            options,
                            now_utc,
                        },
                    );
                }
                return Ok(skipped(routine, "not_due"));
            }
            let walk = record_missed_slots(
                store,
                routine,
                &SkipWindow {
                    cron: &cron,
                    lower_bound: &lower_bound,
                    through: &slot,
                    now_local: &now_local,
                    natural_slot_grace,
                    include_through: true,
                },
                options.dry_run,
            )?;
            Ok(missed_skip_report(routine, &walk))
        }
        DueDecision::NotDue => {
            // No new slot: a most-recent fire that failed (run-level) or
            // errored at dispatch may still have retry budget under the same
            // slot.
            if let Some(retry) = retry_candidate(store, routine, now_utc)? {
                return fire(
                    store,
                    routine,
                    dispatch,
                    FireRequest {
                        slot: &retry.slot,
                        attempt: next_retry_attempt(store, &retry)?,
                        fired_action: "retry_fired",
                        unscheduled: false,
                        options,
                        now_utc,
                    },
                );
            }
            Ok(skipped(routine, "not_due"))
        }
    }
}
