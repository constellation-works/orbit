//! Routine circuit breaker [ORB-25103].
//!
//! After [`BREAKER_TRIP_THRESHOLD`] consecutive fires of one routine fail
//! with the same error class (a failure code or kind, never message text),
//! the sweep opens the routine's breaker and stops firing it. An open breaker
//! admits one trial pass once its backoff elapses, at the first sweep after
//! this host's Orbit build changes (fixes arrive by deploy), or when an
//! operator asks for one fire (bypass), which dispatches a cron routine at
//! once without consuming a scheduled slot. The trial's first fire decides: a
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
/// Trial reason of an operator bypass.
const BYPASS_TRIAL_REASON: &str = "bypass";
/// Actor recorded for transitions the sweep makes.
const SWEEP_ACTOR: &str = "sweep";
/// Newest fires read per source when deciding a trip or a trial.
const OBSERVED_FIRE_LIMIT: usize = 20;

/// Server-computed trial phase of a routine's circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerTrialPhase {
    /// No active or upcoming trial: breaker is closed, or an admitted trial
    /// already finished and awaits the next sweep pass.
    None,
    /// Breaker is open with an upcoming or admitted trial that has not fired
    /// yet.
    Armed,
    /// An admitted trial fire is currently running.
    InFlight,
}

impl BreakerTrialPhase {
    /// Stable lowercase token exposed in status JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Armed => "armed",
            Self::InFlight => "in_flight",
        }
    }
}

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
    /// An operator bypass admitted the trial: fire once now, whether or not
    /// a scheduled slot is due, without consuming one.
    BypassFire,
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

/// Outcome of one gate attempt against a breaker snapshot.
enum GateAttempt {
    /// The gate is decided.
    Done(BreakerGate),
    /// The snapshot changed before the transition landed (a concurrent
    /// operator reset or bypass); re-decide from the fresh row.
    Conflict,
}

/// Decide whether `routine` may be evaluated this pass, tripping, arming,
/// closing or re-opening its breaker as the observed fires require. A dry
/// run writes nothing: it reports an open breaker and never trips one.
/// Transitions are compare-and-swap on the snapshot they decided from: a
/// concurrent operator reset or bypass refuses the stale write and the
/// gate re-decides from the row that is actually stored, so the operator
/// action is never overwritten by this pass.
pub(crate) fn breaker_gate(
    store: &dyn RoutineStoreBackend,
    breakers: &BTreeMap<String, RoutineBreakerRecord>,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    dry_run: bool,
    now_utc: DateTime<Utc>,
) -> Result<BreakerGate, OrbitError> {
    let name = &routine.definition.name;
    let mut current = breakers.get(name).cloned();
    for _ in 0..3 {
        let attempt = match current.as_ref() {
            Some(breaker) if breaker.state == RoutineBreakerState::Open => {
                open_gate(store, breaker, routine, dispatch, dry_run, now_utc)?
            }
            breaker => {
                if !dry_run {
                    maybe_trip(store, breaker, routine, dispatch, now_utc)?
                } else {
                    GateAttempt::Done(BreakerGate::Proceed)
                }
            }
        };
        match attempt {
            GateAttempt::Done(gate) => return Ok(gate),
            GateAttempt::Conflict => {
                current = store.routine_breakers()?.remove(name);
            }
        }
    }
    // Repeated conflicts: hold an open breaker, let a closed one proceed;
    // the next pass decides from a fresh snapshot either way.
    Ok(match current.as_ref() {
        Some(breaker) if breaker.state == RoutineBreakerState::Open => {
            BreakerGate::Skip(BREAKER_OPEN_REASON.into())
        }
        _ => BreakerGate::Proceed,
    })
}

fn maybe_trip(
    store: &dyn RoutineStoreBackend,
    breaker: Option<&RoutineBreakerRecord>,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    now_utc: DateTime<Utc>,
) -> Result<GateAttempt, OrbitError> {
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
        return Ok(GateAttempt::Done(BreakerGate::Proceed));
    }
    let mut classes = failures
        .iter()
        .map(|fire| failure_class(fire, routine, dispatch));
    let Some(class) = classes.next() else {
        return Ok(GateAttempt::Done(BreakerGate::Proceed));
    };
    if !classes.all(|other| other == class) {
        return Ok(GateAttempt::Done(BreakerGate::Proceed));
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
    if !store.routine_breaker_transition_if_current(
        breaker,
        &opened,
        &sweep_event(
            routine,
            "tripped",
            Some(class.clone()),
            last_run_id,
            now_utc,
        ),
    )? {
        return Ok(GateAttempt::Conflict);
    }
    tracing::warn!(
        target: "orbit.core.routines",
        routine = %routine.definition.name,
        error_class = %class,
        "routine circuit breaker tripped after repeated identical failures",
    );
    Ok(GateAttempt::Done(BreakerGate::Skip(
        BREAKER_OPEN_REASON.into(),
    )))
}

fn open_gate(
    store: &dyn RoutineStoreBackend,
    breaker: &RoutineBreakerRecord,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    dry_run: bool,
    now_utc: DateTime<Utc>,
) -> Result<GateAttempt, OrbitError> {
    if let Some(trial_started_at) = breaker.trial_started_at.as_deref() {
        let trial_started_at = parse_rfc3339(trial_started_at)?;
        let fires = observed_fires(store, routine, dispatch)?;
        let Some(trial) = select_trial_fire(&fires, trial_started_at) else {
            // Nothing has fired yet: the trial stays admitted, and a bypass
            // trial keeps asking for its immediate fire.
            return Ok(GateAttempt::Done(admitted_gate(
                breaker.trial_reason.as_deref(),
            )));
        };
        if dry_run {
            return Ok(GateAttempt::Done(match trial.outcome {
                ObservedOutcome::InFlight => {
                    BreakerGate::Skip(BREAKER_TRIAL_IN_FLIGHT_REASON.into())
                }
                _ => BreakerGate::Proceed,
            }));
        }
        return match &trial.outcome {
            ObservedOutcome::InFlight => Ok(GateAttempt::Done(BreakerGate::Skip(
                BREAKER_TRIAL_IN_FLIGHT_REASON.into(),
            ))),
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
                if !store.routine_breaker_transition_if_current(
                    Some(breaker),
                    &closed,
                    &sweep_event(
                        routine,
                        "closed",
                        breaker.trial_reason.clone(),
                        trial.run_id.clone(),
                        now_utc,
                    ),
                )? {
                    return Ok(GateAttempt::Conflict);
                }
                Ok(GateAttempt::Done(BreakerGate::Proceed))
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
                if !store.routine_breaker_transition_if_current(
                    Some(breaker),
                    &reopened,
                    &sweep_event(
                        routine,
                        "reopened",
                        Some(class),
                        trial.run_id.clone(),
                        now_utc,
                    ),
                )? {
                    return Ok(GateAttempt::Conflict);
                }
                Ok(GateAttempt::Done(BreakerGate::Skip(
                    BREAKER_OPEN_REASON.into(),
                )))
            }
            ObservedOutcome::Ignored => Ok(GateAttempt::Done(BreakerGate::Proceed)),
        };
    }

    let Some(reason) = trial_reason(breaker, dispatch, now_utc)? else {
        return Ok(GateAttempt::Done(BreakerGate::Skip(
            BREAKER_OPEN_REASON.into(),
        )));
    };
    if dry_run {
        return Ok(GateAttempt::Done(admitted_gate(Some(reason))));
    }
    let armed = RoutineBreakerRecord {
        trial_started_at: Some(fire_clock_now()),
        trial_reason: Some(reason.into()),
        bypass_requested_at: None,
        updated_at: now_utc.to_rfc3339(),
        source_workspace: routine.source_workspace.clone(),
        ..breaker.clone()
    };
    if !store.routine_breaker_transition_if_current(
        Some(breaker),
        &armed,
        &sweep_event(routine, "trial", Some(reason.into()), None, now_utc),
    )? {
        return Ok(GateAttempt::Conflict);
    }
    Ok(GateAttempt::Done(admitted_gate(Some(reason))))
}

/// The gate of an admitted trial that has not fired yet.
fn admitted_gate(trial_reason: Option<&str>) -> BreakerGate {
    if trial_reason == Some(BYPASS_TRIAL_REASON) {
        BreakerGate::BypassFire
    } else {
        BreakerGate::Proceed
    }
}

/// Why an open breaker without a trial admits one now, if it does.
fn trial_reason(
    breaker: &RoutineBreakerRecord,
    dispatch: &dyn RoutineDispatch,
    now_utc: DateTime<Utc>,
) -> Result<Option<&'static str>, OrbitError> {
    if breaker.bypass_requested_at.is_some() {
        return Ok(Some(BYPASS_TRIAL_REASON));
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

fn is_event_driven(routine: &LoadedRoutine) -> bool {
    let trigger = &routine.definition.trigger;
    trigger.deliveries_landed.is_some() || trigger.state.is_some() || trigger.condition.is_some()
}

/// The first non-ignored fire created after the trial was admitted.
/// `fires` must be ordered newest first.
fn select_trial_fire(
    fires: &[ObservedFire],
    trial_started_at: DateTime<Utc>,
) -> Option<&ObservedFire> {
    fires
        .iter()
        .rev()
        .filter(|fire| fire.created_at > trial_started_at)
        .find(|fire| fire.outcome != ObservedOutcome::Ignored)
}

/// Compute the server-side trial phase and expected fire instant (for an armed
/// trial) using the same observed-fire logic as `open_gate`.
///
/// Event-driven routines (state, delivery, condition) wait for an event rather
/// than a cron slot, so their expected fire instant is `None`.
pub fn breaker_trial_status(
    breaker: Option<&RoutineBreakerRecord>,
    routine: &LoadedRoutine,
    cron_fires: &[RoutineFireRecord],
    automation_fires: &[RoutineFireRecord],
    next_due: Option<&str>,
    now_utc: DateTime<Utc>,
) -> (BreakerTrialPhase, Option<String>) {
    let Some(breaker) = breaker.filter(|breaker| breaker.state == RoutineBreakerState::Open) else {
        return (BreakerTrialPhase::None, None);
    };
    let event_driven = is_event_driven(routine);
    if let Some(trial_started_at) = breaker
        .trial_started_at
        .as_deref()
        .and_then(|raw| parse_rfc3339(raw).ok())
    {
        let automation_runs = if event_driven {
            observed_fires_from_records(automation_fires)
        } else {
            Vec::new()
        };
        let fires = merge_observed_fires(cron_fires, automation_runs);
        return match select_trial_fire(&fires, trial_started_at) {
            Some(fire) if fire.outcome == ObservedOutcome::InFlight => {
                (BreakerTrialPhase::InFlight, None)
            }
            Some(_) => (BreakerTrialPhase::None, None),
            None => {
                let expected_at = if event_driven {
                    None
                } else if breaker.trial_reason.as_deref() == Some(BYPASS_TRIAL_REASON) {
                    Some(now_utc.to_rfc3339())
                } else {
                    next_due.map(str::to_string)
                };
                (BreakerTrialPhase::Armed, expected_at)
            }
        };
    }
    if breaker.bypass_requested_at.is_some() {
        let expected_at = (!event_driven).then(|| now_utc.to_rfc3339());
        return (BreakerTrialPhase::Armed, expected_at);
    }
    let expected_at = (!event_driven)
        .then(|| breaker.next_trial_at.clone())
        .flatten();
    if expected_at.is_some() || event_driven {
        (BreakerTrialPhase::Armed, expected_at)
    } else {
        (BreakerTrialPhase::None, None)
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
    let cron_fires = store.routine_recent_fires(&routine.definition.name, OBSERVED_FIRE_LIMIT)?;
    let automation_runs = if is_event_driven(routine) {
        dispatch.routine_runs(routine, OBSERVED_FIRE_LIMIT)?
    } else {
        Vec::new()
    };
    Ok(merge_observed_fires(&cron_fires, automation_runs))
}

fn observed_fires_from_records(records: &[RoutineFireRecord]) -> Vec<ObservedFire> {
    records
        .iter()
        .take(OBSERVED_FIRE_LIMIT)
        .filter_map(|fire| {
            let created_at = parse_rfc3339(&fire.created_at).ok()?;
            Some(ObservedFire {
                created_at,
                outcome: cron_fire_outcome(fire),
                run_id: fire.run_id.clone(),
            })
        })
        .collect()
}

fn merge_observed_fires(
    cron_fires: &[RoutineFireRecord],
    automation_runs: Vec<ObservedFire>,
) -> Vec<ObservedFire> {
    let mut fires = observed_fires_from_records(cron_fires);
    let runs: HashSet<String> = fires
        .iter()
        .filter_map(|fire| fire.run_id.clone())
        .collect();
    fires.extend(
        automation_runs
            .into_iter()
            .take(OBSERVED_FIRE_LIMIT)
            .filter(|fire| fire.run_id.as_ref().is_none_or(|id| !runs.contains(id))),
    );
    fires.sort_by_key(|fire| std::cmp::Reverse(fire.created_at));
    fires
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
