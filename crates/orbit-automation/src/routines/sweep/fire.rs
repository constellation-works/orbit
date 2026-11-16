use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{RoutineFireIntentParams, RoutineFireState, RoutineStoreBackend};
use orbit_types::workflow::JobRunState;
use orbit_types::workflow::OverlapPolicy;
use std::collections::BTreeMap;
use std::time::Instant;

use crate::condition::GuardVerdict;
use orbit_types::workflow::automation::ConditionGuardMiss;

use super::super::loader::LoadedRoutine;
use super::contract::{RoutineDispatch, RoutineSweepReport, RunOwnerLiveness, SweepOptions};
use super::core::BREAKER_BYPASS_FIRE_REASON;
use super::report::{action, duration_from_minutes, parse_rfc3339, skipped};

pub(super) struct FireRequest<'a> {
    pub(super) slot: &'a str,
    pub(super) attempt: u32,
    pub(super) fired_action: &'static str,
    /// A breaker-bypass fire: it claims no scheduled slot, so it neither
    /// advances the cursor nor consumes the slot on a guard miss.
    pub(super) unscheduled: bool,
    pub(super) options: SweepOptions,
    pub(super) now_utc: DateTime<Utc>,
}

pub(super) fn fire(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    request: FireRequest<'_>,
) -> Result<RoutineSweepReport, OrbitError> {
    let FireRequest {
        slot,
        attempt,
        fired_action,
        unscheduled,
        options,
        now_utc,
    } = request;
    let definition = &routine.definition;
    let name = &definition.name;

    if definition.policy.overlap == OverlapPolicy::Forbid
        && store.routine_blocking_fire(name)?.is_some()
    {
        // Stale in-flight entries past the policy timeout were already
        // reclaimed by the outcome sync at the top of the pass, so anything
        // still non-terminal here is genuinely (believed) in flight.
        return Ok(RoutineSweepReport {
            slot: Some(slot.to_string()),
            ..skipped(routine, "overlap_in_flight")
        });
    }

    if let GuardVerdict::Closed { reason, detail } = dispatch.evaluate_guard(routine, now_utc) {
        let consume = definition.trigger.guard_miss == Some(ConditionGuardMiss::Skip)
            && fired_action == "fired"
            && !unscheduled;
        if consume && !options.dry_run {
            // `guard_miss: skip` consumes the slot as a recorded skip.
            let claimed = store.routine_record_fire_intent(&RoutineFireIntentParams {
                routine_name: name.clone(),
                slot: slot.to_string(),
                attempt,
                source_workspace: routine.source_workspace.clone(),
            })?;
            if claimed {
                store.routine_mark_fire_outcome(
                    name,
                    slot,
                    attempt,
                    RoutineFireState::Skipped,
                    Some(&format!("{reason}: {detail}")),
                )?;
            }
        }
        return Ok(RoutineSweepReport {
            slot: Some(slot.to_string()),
            ..skipped(routine, &format!("{reason}: {detail}"))
        });
    }

    if options.dry_run {
        return Ok(RoutineSweepReport {
            slot: Some(slot.to_string()),
            ..action(routine, "would_fire")
        });
    }

    let intent = RoutineFireIntentParams {
        routine_name: name.clone(),
        slot: slot.to_string(),
        attempt,
        source_workspace: routine.source_workspace.clone(),
    };
    let claimed = if unscheduled {
        store.routine_record_unscheduled_fire_intent(&intent)?
    } else {
        store.routine_record_fire_intent(&intent)?
    };
    if !claimed {
        return Ok(RoutineSweepReport {
            slot: Some(slot.to_string()),
            ..skipped(routine, "slot_already_claimed")
        });
    }

    let actor = format!("routine/{name}");
    match dispatch.submit(
        &routine.source_orbit_dir,
        definition.target.job_name(),
        &actor,
        slot,
    ) {
        Ok(run_id) => {
            store.routine_mark_fire_dispatched(name, slot, attempt, &run_id)?;
            Ok(RoutineSweepReport {
                routine: name.clone(),
                source: routine.source_workspace.clone(),
                origin: routine.origin.as_str(),
                action: fired_action,
                reason: unscheduled.then(|| BREAKER_BYPASS_FIRE_REASON.to_string()),
                slot: Some(slot.to_string()),
                run_id: Some(run_id),
                batch: Vec::new(),
            })
        }
        Err(error) => {
            // A synchronous dispatch failure means nothing was dispatched, so
            // record it as `Failed` — retryable under the same slot within
            // `policy.retries` — rather than the terminal `Error`
            // the outcome sync reserves for an ambiguous crash-orphaned intent.
            store.routine_mark_fire_outcome(
                name,
                slot,
                attempt,
                RoutineFireState::Failed,
                Some(&format!("dispatch failed: {error}")),
            )?;
            Ok(RoutineSweepReport {
                routine: name.clone(),
                source: routine.source_workspace.clone(),
                origin: routine.origin.as_str(),
                action: "error",
                reason: Some(format!("dispatch failed: {error}")),
                slot: Some(slot.to_string()),
                run_id: None,
                batch: Vec::new(),
            })
        }
    }
}

/// Bring unresolved fires up to date against actual run state, and reclaim
/// entries older than the routine's policy timeout (the staleness horizon —
/// without it, a sweep that crashed between intent and dispatch would block
/// `overlap: forbid` forever).
pub(super) fn sync_unresolved_fires(
    store: &dyn RoutineStoreBackend,
    routines_by_name: &BTreeMap<String, &LoadedRoutine>,
    dispatch: &dyn RoutineDispatch,
    now_utc: DateTime<Utc>,
    deadline: Instant,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let mut errors = BTreeMap::new();
    for fire in store.routine_unresolved_fires()? {
        if Instant::now() >= deadline {
            break;
        }
        // A fire recorded for a routine this host no longer loads must leave
        // that prior history untouched.
        let Some(routine) = routines_by_name.get(&fire.routine_name) else {
            continue;
        };
        let timeout = match duration_from_minutes(
            routine.definition.policy.timeout_minutes,
            "policy.timeout_minutes",
        ) {
            Ok(timeout) => timeout,
            Err(error) => {
                errors
                    .entry(routine.definition.name.clone())
                    .or_insert_with(|| error.to_string());
                continue;
            }
        };
        let created_at = match parse_rfc3339(&fire.created_at) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let expired = now_utc.signed_duration_since(created_at) > timeout;

        match fire.state {
            // A recorded intent whose sweep died before dispatch: reclaim it
            // once past the timeout horizon, otherwise leave it for a later pass.
            RoutineFireState::Intent if expired => {
                store.routine_mark_fire_outcome(
                    &fire.routine_name,
                    &fire.slot,
                    fire.attempt,
                    RoutineFireState::Error,
                    Some("stale fire intent reclaimed (sweep died before dispatch)"),
                )?;
            }
            RoutineFireState::Dispatched => {
                let run_id = fire.run_id.as_deref();
                let run_state =
                    run_id.and_then(|run_id| dispatch.run_state(&routine.source_orbit_dir, run_id));
                // Reclaiming a fire past the policy timeout is what keeps a
                // routine from wedging forever; it is also the only sanctioned
                // way an `overlap:forbid` slot is freed while work may still be
                // in flight.
                let timed_out = || {
                    expired.then_some((
                        RoutineFireState::TimedOut,
                        Some("exceeded policy timeout without a terminal run state"),
                    ))
                };
                let outcome = match run_state {
                    Some(JobRunState::Success) => Some((RoutineFireState::Succeeded, None)),
                    Some(JobRunState::Failed) => Some((RoutineFireState::Failed, None)),
                    Some(JobRunState::Timeout) => Some((RoutineFireState::TimedOut, None)),
                    // Cancellation signals the owner and verifies its
                    // termination, so a cancelled run has genuinely stopped.
                    Some(JobRunState::Cancelled) => {
                        Some((RoutineFireState::Failed, Some("run cancelled")))
                    }
                    // A persisted in-flight state is not enough to hold an
                    // overlap slot after a restart. If the recorded owner is
                    // conclusively gone, no work can still be executing and
                    // the fire can be reconciled immediately. Alive and
                    // unknown owners remain protected until their terminal
                    // state or the normal policy timeout.
                    Some(JobRunState::Running | JobRunState::Retrying) => {
                        let liveness = run_id.map_or(RunOwnerLiveness::Unknown, |run_id| {
                            dispatch.run_owner_liveness(&routine.source_orbit_dir, run_id)
                        });
                        match liveness {
                            RunOwnerLiveness::Stopped => Some((
                                RoutineFireState::Failed,
                                Some("run owner stopped before recording a terminal outcome"),
                            )),
                            RunOwnerLiveness::Alive | RunOwnerLiveness::Unknown => timed_out(),
                        }
                    }
                    // [ORB-10597] Resolving a fire is what releases the
                    // `overlap:forbid` slot, and for `interrupted` alone the
                    // terminal state is not evidence that the work stopped:
                    // marking a run interrupted attaches no teardown, so a run
                    // condemned in error keeps executing. Releasing the slot
                    // then admits a second instance against the same surface
                    // while the first is still working.
                    //
                    // The distinction to draw is terminal-*and-stopped* versus
                    // terminal-*and-still-executing*, which the run's recorded
                    // owner answers. A stopped owner releases exactly as before
                    // — that case is correct and is why this arm cannot simply
                    // be deleted. An owner that is alive, or that this host
                    // cannot conclusively probe, is treated as still in flight:
                    // the slot stays held and the fire is reclaimed only by the
                    // policy timeout, the same bound every genuinely in-flight
                    // run already lives under. That bound is what keeps an
                    // unprobeable owner from wedging the routine forever.
                    Some(JobRunState::Interrupted) => {
                        let liveness = run_id.map_or(RunOwnerLiveness::Unknown, |run_id| {
                            dispatch.run_owner_liveness(&routine.source_orbit_dir, run_id)
                        });
                        match liveness {
                            RunOwnerLiveness::Stopped => {
                                Some((RoutineFireState::Failed, Some("run interrupted")))
                            }
                            RunOwnerLiveness::Alive | RunOwnerLiveness::Unknown => {
                                tracing::warn!(
                                    target: "orbit.core.routines",
                                    routine = %fire.routine_name,
                                    slot = %fire.slot,
                                    run_id = run_id.unwrap_or("-"),
                                    liveness = ?liveness,
                                    "routine run is marked interrupted but its worker has not \
                                     been shown to have stopped; holding the overlap slot",
                                );
                                timed_out()
                            }
                        }
                    }
                    // Still in flight (or unqueryable): reclaim once past the
                    // policy timeout, otherwise leave for a later pass.
                    _ => timed_out(),
                };

                if let Some((state, detail)) = outcome {
                    store.routine_mark_fire_outcome(
                        &fire.routine_name,
                        &fire.slot,
                        fire.attempt,
                        state,
                        detail,
                    )?;
                }
            }
            _ => {}
        }
    }

    Ok(errors)
}
