//! Routine circuit breaker [ORB-25103].
//!
//! After [`BREAKER_TRIP_THRESHOLD`] consecutive fires of one routine fail
//! with the same error class (a failure code or kind, never message text),
//! the sweep opens the routine's breaker and stops firing it. An open breaker
//! admits one trial pass once its backoff elapses, at the first sweep after
//! this host's Orbit build changes (fixes arrive by deploy), or when an
//! operator asks for one fire (bypass). The trial's first fire decides: a
//! success closes the breaker, a failure re-opens it with a doubled backoff
//! capped at [`BREAKER_MAX_BACKOFF_MINUTES`]. An operator reset closes it at
//! once, and failures before the reset no longer count toward a new trip.
//!
//! Every transition and operator action is appended to the host-local
//! breaker log, so a trip is one recorded event rather than one per fire.

use super::loader::LoadedRoutine;
use super::sweep::RoutineDispatch;
use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    RoutineBreakerEvent, RoutineBreakerRecord, RoutineBreakerState, RoutineFireRecord,
    RoutineFireState, RoutineStoreBackend,
};
use std::collections::{BTreeMap, HashSet};

/// Consecutive same-class failures that trip a routine's breaker.
pub const BREAKER_TRIP_THRESHOLD: usize = 3;
/// Backoff before the first trial after a trip.
pub const BREAKER_BASE_BACKOFF_MINUTES: i64 = 15;
/// Longest backoff between trials, however often the breaker re-opens.
pub const BREAKER_MAX_BACKOFF_MINUTES: i64 = 240;
/// Skip reason of a routine whose breaker is open.
pub const BREAKER_OPEN_REASON: &str = "breaker_open";
/// Skip reason of a routine whose trial fire is still in flight.
pub const BREAKER_TRIAL_IN_FLIGHT_REASON: &str = "breaker_trial_in_flight";
/// Actor recorded for transitions the sweep makes.
const SWEEP_ACTOR: &str = "sweep";
/// Newest fires read per source when deciding a trip or a trial.
const OBSERVED_FIRE_LIMIT: usize = 20;

/// One fire of a routine as the breaker sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedFire {
    /// When the fire was created (cron intent or run creation).
    pub created_at: DateTime<Utc>,
    /// Its outcome so far.
    pub outcome: ObservedOutcome,
    /// The run it dispatched, when one was.
    pub run_id: Option<String>,
}

/// Outcome of one observed fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservedOutcome {
    /// Not finished yet.
    InFlight,
    /// Finished successfully.
    Succeeded,
    /// Failed with this error class, when already known.
    Failed(Option<String>),
    /// Neither a success nor a failure (a skip); ignored.
    Ignored,
}

/// What the breaker lets this pass do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakerGate {
    /// Evaluate the routine as usual.
    Proceed,
    /// Skip the routine this pass for this reason.
    Skip(String),
}

/// Backoff before the trial that follows the `trips`-th trip.
pub fn breaker_backoff(trips: u32) -> Duration {
    let doublings = trips.saturating_sub(1).min(16);
    let minutes = BREAKER_BASE_BACKOFF_MINUTES
        .saturating_mul(1_i64 << doublings)
        .min(BREAKER_MAX_BACKOFF_MINUTES);
    Duration::minutes(minutes)
}

/// Decide whether `routine` may be evaluated this pass, tripping, arming,
/// closing or re-opening its breaker as the observed fires require. A dry
/// run writes nothing: it reports an open breaker and never trips one.
pub(crate) fn breaker_gate(
    store: &dyn RoutineStoreBackend,
    breakers: &BTreeMap<String, RoutineBreakerRecord>,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    dry_run: bool,
    now_utc: DateTime<Utc>,
) -> Result<BreakerGate, OrbitError> {
    let name = &routine.definition.name;
    let breaker = breakers.get(name);
    match breaker {
        Some(breaker) if breaker.state == RoutineBreakerState::Open => {
            open_gate(store, breaker, routine, dispatch, dry_run, now_utc)
        }
        _ => {
            if !dry_run {
                maybe_trip(store, breaker, routine, dispatch, now_utc)
            } else {
                Ok(BreakerGate::Proceed)
            }
        }
    }
}

fn maybe_trip(
    store: &dyn RoutineStoreBackend,
    breaker: Option<&RoutineBreakerRecord>,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    now_utc: DateTime<Utc>,
) -> Result<BreakerGate, OrbitError> {
    let floor = breaker
        .and_then(|breaker| breaker.floor_at.as_deref())
        .map(parse_rfc3339)
        .transpose()?;
    let fires = observed_fires(store, routine, dispatch)?;
    let failures: Vec<&ObservedFire> = fires
        .iter()
        .filter(|fire| floor.is_none_or(|floor| fire.created_at > floor))
        // A skipped fire is not a failure and may be ignored. An in-flight
        // fire is still part of the streak: it must finish before older
        // failures can be called consecutive.
        .filter(|fire| fire.outcome != ObservedOutcome::Ignored)
        .take_while(|fire| matches!(fire.outcome, ObservedOutcome::Failed(_)))
        .take(BREAKER_TRIP_THRESHOLD)
        .collect();
    if failures.len() < BREAKER_TRIP_THRESHOLD {
        return Ok(BreakerGate::Proceed);
    }
    let mut classes = failures
        .iter()
        .map(|fire| failure_class(fire, routine, dispatch));
    let Some(class) = classes.next() else {
        return Ok(BreakerGate::Proceed);
    };
    if !classes.all(|other| other == class) {
        return Ok(BreakerGate::Proceed);
    }
    let last_run_id = failures.iter().find_map(|fire| fire.run_id.clone());
    let opened = opened_breaker(
        routine,
        breaker,
        1,
        &class,
        last_run_id.clone(),
        dispatch,
        now_utc,
    );
    store.routine_breaker_transition(
        &opened,
        &sweep_event(
            routine,
            "tripped",
            Some(class.clone()),
            last_run_id,
            now_utc,
        ),
    )?;
    tracing::warn!(
        target: "orbit.core.routines",
        routine = %routine.definition.name,
        error_class = %class,
        "routine circuit breaker tripped after repeated identical failures",
    );
    Ok(BreakerGate::Skip(BREAKER_OPEN_REASON.into()))
}

fn open_gate(
    store: &dyn RoutineStoreBackend,
    breaker: &RoutineBreakerRecord,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    dry_run: bool,
    now_utc: DateTime<Utc>,
) -> Result<BreakerGate, OrbitError> {
    if let Some(trial_started_at) = breaker.trial_started_at.as_deref() {
        let trial_started_at = parse_rfc3339(trial_started_at)?;
        let fires = observed_fires(store, routine, dispatch)?;
        // The trial is the first fire created after it was admitted.
        let trial = fires
            .iter()
            .rev()
            .filter(|fire| fire.created_at > trial_started_at)
            .find(|fire| fire.outcome != ObservedOutcome::Ignored);
        let Some(trial) = trial else {
            // Nothing has fired yet: the trial stays admitted.
            return Ok(BreakerGate::Proceed);
        };
        if dry_run {
            return Ok(match trial.outcome {
                ObservedOutcome::InFlight => {
                    BreakerGate::Skip(BREAKER_TRIAL_IN_FLIGHT_REASON.into())
                }
                _ => BreakerGate::Proceed,
            });
        }
        return match &trial.outcome {
            ObservedOutcome::InFlight => {
                Ok(BreakerGate::Skip(BREAKER_TRIAL_IN_FLIGHT_REASON.into()))
            }
            ObservedOutcome::Succeeded => {
                let closed = RoutineBreakerRecord {
                    state: RoutineBreakerState::Closed,
                    trips: 0,
                    next_trial_at: None,
                    trial_started_at: None,
                    trial_reason: None,
                    bypass_requested_at: None,
                    floor_at: Some(fire_clock_now()),
                    updated_at: now_utc.to_rfc3339(),
                    source_workspace: routine.source_workspace.clone(),
                    ..breaker.clone()
                };
                store.routine_breaker_transition(
                    &closed,
                    &sweep_event(
                        routine,
                        "closed",
                        breaker.trial_reason.clone(),
                        trial.run_id.clone(),
                        now_utc,
                    ),
                )?;
                Ok(BreakerGate::Proceed)
            }
            ObservedOutcome::Failed(_) => {
                let class = failure_class(trial, routine, dispatch);
                let trips = breaker.trips.saturating_add(1);
                let reopened = opened_breaker(
                    routine,
                    Some(breaker),
                    trips,
                    &class,
                    trial.run_id.clone(),
                    dispatch,
                    now_utc,
                );
                store.routine_breaker_transition(
                    &reopened,
                    &sweep_event(
                        routine,
                        "reopened",
                        Some(class),
                        trial.run_id.clone(),
                        now_utc,
                    ),
                )?;
                Ok(BreakerGate::Skip(BREAKER_OPEN_REASON.into()))
            }
            ObservedOutcome::Ignored => Ok(BreakerGate::Proceed),
        };
    }

    let Some(reason) = trial_reason(breaker, dispatch, now_utc)? else {
        return Ok(BreakerGate::Skip(BREAKER_OPEN_REASON.into()));
    };
    if dry_run {
        return Ok(BreakerGate::Proceed);
    }
    let armed = RoutineBreakerRecord {
        trial_started_at: Some(fire_clock_now()),
        trial_reason: Some(reason.into()),
        bypass_requested_at: None,
        updated_at: now_utc.to_rfc3339(),
        source_workspace: routine.source_workspace.clone(),
        ..breaker.clone()
    };
    store.routine_breaker_transition(
        &armed,
        &sweep_event(routine, "trial", Some(reason.into()), None, now_utc),
    )?;
    Ok(BreakerGate::Proceed)
}

/// Why an open breaker without a trial admits one now, if it does.
fn trial_reason(
    breaker: &RoutineBreakerRecord,
    dispatch: &dyn RoutineDispatch,
    now_utc: DateTime<Utc>,
) -> Result<Option<&'static str>, OrbitError> {
    if breaker.bypass_requested_at.is_some() {
        return Ok(Some("bypass"));
    }
    if let (Some(tripped_on), Some(current)) =
        (breaker.orbit_build.as_deref(), dispatch.host_orbit_build())
        && tripped_on != current
    {
        return Ok(Some("version"));
    }
    let due = match breaker.next_trial_at.as_deref() {
        Some(at) => now_utc >= parse_rfc3339(at)?,
        None => true,
    };
    Ok(due.then_some("backoff"))
}

fn opened_breaker(
    routine: &LoadedRoutine,
    previous: Option<&RoutineBreakerRecord>,
    trips: u32,
    class: &str,
    last_run_id: Option<String>,
    dispatch: &dyn RoutineDispatch,
    now_utc: DateTime<Utc>,
) -> RoutineBreakerRecord {
    RoutineBreakerRecord {
        routine_name: routine.definition.name.clone(),
        source_workspace: routine.source_workspace.clone(),
        state: RoutineBreakerState::Open,
        error_class: Some(class.to_string()),
        last_run_id,
        tripped_at: Some(now_utc.to_rfc3339()),
        trips,
        next_trial_at: Some((now_utc + breaker_backoff(trips)).to_rfc3339()),
        orbit_build: dispatch.host_orbit_build(),
        trial_started_at: None,
        trial_reason: None,
        bypass_requested_at: None,
        floor_at: previous.and_then(|previous| previous.floor_at.clone()),
        updated_at: now_utc.to_rfc3339(),
    }
}

fn sweep_event(
    routine: &LoadedRoutine,
    event: &str,
    detail: Option<String>,
    run_id: Option<String>,
    now_utc: DateTime<Utc>,
) -> RoutineBreakerEvent {
    RoutineBreakerEvent {
        routine_name: routine.definition.name.clone(),
        event: event.into(),
        actor: SWEEP_ACTOR.into(),
        detail,
        run_id,
        at: now_utc.to_rfc3339(),
    }
}

/// The routine's newest fires, newest first: its cron fire rows and, for a
/// state-, delivery- or condition-triggered routine, the runs its automation
/// admitted. A run present in both is counted once.
fn observed_fires(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
) -> Result<Vec<ObservedFire>, OrbitError> {
    let mut fires = Vec::new();
    let mut runs = HashSet::new();
    for fire in store.routine_recent_fires(&routine.definition.name, OBSERVED_FIRE_LIMIT)? {
        let Ok(created_at) = parse_rfc3339(&fire.created_at) else {
            continue;
        };
        if let Some(run_id) = &fire.run_id {
            runs.insert(run_id.clone());
        }
        fires.push(ObservedFire {
            created_at,
            outcome: cron_fire_outcome(&fire),
            run_id: fire.run_id.clone(),
        });
    }
    let trigger = &routine.definition.trigger;
    if trigger.deliveries_landed.is_some() || trigger.state.is_some() || trigger.condition.is_some()
    {
        fires.extend(
            dispatch
                .routine_runs(routine, OBSERVED_FIRE_LIMIT)?
                .into_iter()
                .filter(|fire| fire.run_id.as_ref().is_none_or(|id| !runs.contains(id))),
        );
    }
    fires.sort_by_key(|fire| std::cmp::Reverse(fire.created_at));
    Ok(fires)
}

fn cron_fire_outcome(fire: &RoutineFireRecord) -> ObservedOutcome {
    match fire.state {
        RoutineFireState::Intent | RoutineFireState::Dispatched => ObservedOutcome::InFlight,
        RoutineFireState::Succeeded => ObservedOutcome::Succeeded,
        RoutineFireState::Skipped => ObservedOutcome::Ignored,
        RoutineFireState::TimedOut => ObservedOutcome::Failed(Some("timed_out".into())),
        RoutineFireState::Error => ObservedOutcome::Failed(Some("fire_error".into())),
        RoutineFireState::Failed if fire.run_id.is_none() => {
            ObservedOutcome::Failed(Some("dispatch_failed".into()))
        }
        RoutineFireState::Failed => ObservedOutcome::Failed(None),
    }
}

/// The error class of a failed fire: its known class, else the failed run's
/// recorded failure code, else plain `failed`.
fn failure_class(
    fire: &ObservedFire,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
) -> String {
    if let ObservedOutcome::Failed(Some(class)) = &fire.outcome {
        return class.clone();
    }
    fire.run_id
        .as_deref()
        .and_then(|run_id| dispatch.run_failure_class(&routine.source_orbit_dir, run_id))
        .unwrap_or_else(|| "failed".into())
}

/// Now on the clock fire rows and runs are stamped with. Trial and floor
/// markers are compared with those stamps, not with the sweep's `now`, which
/// a caller may supply.
fn fire_clock_now() -> String {
    Utc::now().to_rfc3339()
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}
