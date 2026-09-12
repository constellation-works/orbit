//! Deterministic routine evaluation, retry and overlap coordination.

use super::due::{
    DueDecision, due_decision_with_grace, natural_slot_grace_for_cadence, parse_cron,
};
use super::loader::{LoadedRoutine, RoutineCollection, RoutineLoadError};
use crate::host::RunOwnerLiveness;
use chrono::{DateTime, Duration, Local, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    RoutineFireIntentParams, RoutineFireRecord, RoutineFireState, RoutineStoreBackend,
};
use orbit_types::workflow::{JobRunState, OverlapPolicy};
use std::collections::BTreeMap;
use std::path::Path;

/// Dispatch seam for the sweep. The production impl ([`super::tick`]) wraps
/// one [`AutomationHost`](crate::host::AutomationHost) per source workspace;
/// tests supply a fake so the sweep's fire / retry / overlap / outcome-sync
/// orchestration is exercised deterministically without spawning pipeline
/// workers.
pub trait RoutineDispatch {
    fn evaluate_delivery(
        &self,
        _routine: &LoadedRoutine,
        _dry_run: bool,
        _now: DateTime<Utc>,
    ) -> Result<orbit_types::workflow::automation::AutomationDiagnostic, OrbitError> {
        Err(OrbitError::Execution(
            "delivery source adapter unavailable".into(),
        ))
    }

    /// Submit `job_name` in the source workspace rooted at `source_orbit_dir`
    /// under `actor`, returning the dispatched run id. `slot` is the RFC 3339
    /// scheduled slot consumed by this fire [ORB-12255].
    fn submit(
        &self,
        source_orbit_dir: &Path,
        job_name: &str,
        actor: &str,
        slot: &str,
    ) -> Result<String, OrbitError>;

    /// Current run state for a dispatched fire, when the run is queryable.
    fn run_state(&self, source_orbit_dir: &Path, run_id: &str) -> Option<JobRunState>;

    /// [ORB-10597] Whether the run's recorded owner process is still executing,
    /// asked independently of its persisted state. Consulted only for runs
    /// marked `interrupted`, which carries no teardown and so is not evidence
    /// that the work stopped.
    fn run_owner_liveness(&self, source_orbit_dir: &Path, run_id: &str) -> RunOwnerLiveness;
}

/// Options for one sweep pass.
#[derive(Debug, Clone, Copy)]
pub struct SweepOptions {
    /// Report what would fire without recording or dispatching anything.
    pub dry_run: bool,
    /// Cadence of the host clock that invokes this pass. The production
    /// assembly supplies the configured clock cadence; deterministic callers
    /// use the compatible 60-second default.
    pub sweep_cadence_seconds: u64,
}

impl Default for SweepOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            sweep_cadence_seconds: 60,
        }
    }
}

/// Per-routine outcome of one sweep pass.
#[derive(Debug, Clone)]
pub struct RoutineSweepReport {
    /// Routine name.
    pub routine: String,
    /// Source workspace name.
    pub source: String,
    /// Whether the definition is `committed` or `local` origin.
    pub origin: &'static str,
    /// One of: `fired`, `retry_fired`, `would_fire`, `baselined`,
    /// `would_baseline`, `skipped`, `error`.
    pub action: &'static str,
    /// Why, for `skipped`/`error` rows.
    pub reason: Option<String>,
    /// Scheduled slot consumed (RFC 3339, UTC), when a fire was involved.
    pub slot: Option<String>,
    /// Run id returned by dispatch, when one was submitted.
    pub run_id: Option<String>,
}

/// Per-auto-task outcome included in the host tick report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoTaskSweepReport {
    /// Auto-task definition name, or the failing file name for a load error.
    pub name: String,
    /// Source workspace name.
    pub source: String,
    /// One of: `minted`, `would_fire`, `baselined`, `would_baseline`,
    /// `skipped`, `error`.
    pub action: &'static str,
    /// Why, for `skipped` and `error` rows.
    pub reason: Option<String>,
    /// Scheduled slot consumed (RFC 3339, UTC), when a fire was involved.
    pub slot: Option<String>,
    /// Task minted by this tick.
    pub task_id: Option<String>,
}

/// Result of one sweep pass.
#[derive(Debug, Default)]
pub struct SweepOutcome {
    /// Display identity of the host this pass ran on.
    pub host_id: String,
    /// Stable machine identity of the host this pass ran on.
    pub machine_id: String,
    /// True when another sweep held the lock and this pass exited early.
    pub lock_busy: bool,
    /// Per-routine outcomes.
    pub reports: Vec<RoutineSweepReport>,
    /// Per-auto-task outcomes, evaluated after all routine rows.
    pub auto_task_reports: Vec<AutoTaskSweepReport>,
    /// Fail-closed definition/load failures (those routines were absent).
    pub load_errors: Vec<RoutineLoadError>,
    /// Set when every discovered workspace failed to open, so this pass
    /// loaded nothing. The CLI prints this one row and exits non-zero.
    /// Partial load errors leave this `None`.
    pub no_workspace_loaded: Option<String>,
}

/// The dispatch-agnostic core of one sweep pass: outcome-sync
/// (unless dry-run), then per-routine due evaluation and fire/skip. Split out
/// from the host pass in [`super::tick`] — which owns the lock, store, and
/// workspace discovery — so the orchestration can be driven against a temp
/// store, a hand-built [`RoutineCollection`], a fake [`RoutineDispatch`], and
/// an explicit `now`.
pub fn run_sweep_core(
    store: &dyn RoutineStoreBackend,
    collection: &RoutineCollection,
    dispatch: &dyn RoutineDispatch,
    options: SweepOptions,
    now_utc: DateTime<Utc>,
) -> Result<Vec<RoutineSweepReport>, OrbitError> {
    let routines_by_name: BTreeMap<String, &LoadedRoutine> = collection
        .routines
        .iter()
        .map(|routine| (routine.definition.name.clone(), routine))
        .collect();

    let sync_errors = if options.dry_run {
        BTreeMap::new()
    } else {
        sync_unresolved_fires(store, &routines_by_name, dispatch, now_utc)?
    };

    let pauses = store.routine_pauses()?;

    let mut reports = Vec::new();
    for routine in &collection.routines {
        let same_target = collection
            .routines
            .iter()
            .filter(|other| {
                other.source_orbit_dir == routine.source_orbit_dir
                    && other.definition.target == routine.definition.target
                    && other.definition.enabled
            })
            .collect::<Vec<_>>();
        if same_target.len() > 1
            && same_target
                .iter()
                .any(|other| other.definition.trigger.state.is_some())
        {
            reports.push(skipped(routine, "duplicate_routine_ownership"));
            continue;
        }
        if let Some(reason) = sync_errors.get(&routine.definition.name) {
            reports.push(failure_report(routine, reason.clone()));
            continue;
        }
        let report = sweep_routine(store, routine, dispatch, &pauses, options, now_utc)
            .unwrap_or_else(|error| failure_report(routine, error.to_string()));
        reports.push(report);
    }

    Ok(reports)
}

fn sweep_routine(
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
    match due_decision_with_grace(
        &cron,
        definition.trigger.missed_run,
        &lower_bound,
        &now_local,
        natural_slot_grace,
    )? {
        DueDecision::Fire { slot, .. } => {
            let slot_utc = slot.with_timezone(&Utc).to_rfc3339();
            fire(
                store,
                routine,
                dispatch,
                FireRequest {
                    slot: &slot_utc,
                    attempt: 1,
                    fired_action: "fired",
                    options,
                },
            )
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
                        attempt: retry.attempt + 1,
                        fired_action: "retry_fired",
                        options,
                    },
                );
            }
            Ok(skipped(routine, "not_due"))
        }
    }
}

/// The most recent fire, when it failed with retry budget left and the fixed
/// backoff has elapsed.
///
/// Retryable means `Failed` — a run-level failure *or* a synchronous dispatch
/// failure: `fire` records a `submit_pipeline_run` that returns
/// `Err` as `Failed` (not `Error`) precisely because nothing dispatched, so it
/// is unambiguously safe to re-dispatch under the same slot. `Error` is
/// reserved for the *ambiguous* case — a crashed sweep's stale intent reclaimed
/// by the outcome sync, where a worker may have partially started — and stays
/// terminal so a make-up fire never races an orphaned run.
fn retry_candidate(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
    now_utc: DateTime<Utc>,
) -> Result<Option<RoutineFireRecord>, OrbitError> {
    let retries = routine.definition.policy.retries;
    if retries.max == 0 {
        return Ok(None);
    }
    let Some(latest) = store.routine_latest_fire(&routine.definition.name)? else {
        return Ok(None);
    };
    if latest.state != RoutineFireState::Failed {
        return Ok(None);
    }
    // attempt is 1-based: max=2 allows attempts 2 and 3.
    if latest.attempt > retries.max {
        return Ok(None);
    }
    let failed_at = parse_rfc3339(&latest.updated_at)?;
    if now_utc.signed_duration_since(failed_at)
        < duration_from_minutes(retries.backoff_minutes, "policy.retries.backoff_minutes")?
    {
        return Ok(None);
    }
    Ok(Some(latest))
}

struct FireRequest<'a> {
    slot: &'a str,
    attempt: u32,
    fired_action: &'static str,
    options: SweepOptions,
}

fn fire(
    store: &dyn RoutineStoreBackend,
    routine: &LoadedRoutine,
    dispatch: &dyn RoutineDispatch,
    request: FireRequest<'_>,
) -> Result<RoutineSweepReport, OrbitError> {
    let FireRequest {
        slot,
        attempt,
        fired_action,
        options,
    } = request;
    let definition = &routine.definition;
    let name = &definition.name;

    if definition.policy.overlap == OverlapPolicy::Forbid
        && let Some(latest) = store.routine_latest_fire(name)?
        && !latest.state.is_terminal()
    {
        // Stale in-flight entries past the policy timeout were already
        // reclaimed by the outcome sync at the top of the pass, so anything
        // still non-terminal here is genuinely (believed) in flight.
        return Ok(RoutineSweepReport {
            slot: Some(slot.to_string()),
            ..skipped(routine, "overlap_in_flight")
        });
    }

    if options.dry_run {
        return Ok(RoutineSweepReport {
            slot: Some(slot.to_string()),
            ..action(routine, "would_fire")
        });
    }

    let claimed = store.routine_record_fire_intent(&RoutineFireIntentParams {
        routine_name: name.clone(),
        slot: slot.to_string(),
        attempt,
        source_workspace: routine.source_workspace.clone(),
    })?;
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
                reason: None,
                slot: Some(slot.to_string()),
                run_id: Some(run_id),
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
            })
        }
    }
}

/// Bring unresolved fires up to date against actual run state, and reclaim
/// entries older than the routine's policy timeout (the staleness horizon —
/// without it, a sweep that crashed between intent and dispatch would block
/// `overlap: forbid` forever).
fn sync_unresolved_fires(
    store: &dyn RoutineStoreBackend,
    routines_by_name: &BTreeMap<String, &LoadedRoutine>,
    dispatch: &dyn RoutineDispatch,
    now_utc: DateTime<Utc>,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let mut errors = BTreeMap::new();
    for fire in store.routine_unresolved_fires()? {
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

fn duration_from_minutes(minutes: u64, field: &str) -> Result<Duration, OrbitError> {
    let minutes = i64::try_from(minutes).map_err(|_| {
        OrbitError::InvalidInput(format!("routine {field} is too large for a duration"))
    })?;

    Duration::try_minutes(minutes).ok_or_else(|| {
        OrbitError::InvalidInput(format!("routine {field} is too large for a duration"))
    })
}

fn skipped(routine: &LoadedRoutine, reason: &str) -> RoutineSweepReport {
    RoutineSweepReport {
        routine: routine.definition.name.clone(),
        source: routine.source_workspace.clone(),
        origin: routine.origin.as_str(),
        action: "skipped",
        reason: Some(reason.to_string()),
        slot: None,
        run_id: None,
    }
}

fn failure_report(routine: &LoadedRoutine, reason: String) -> RoutineSweepReport {
    RoutineSweepReport {
        routine: routine.definition.name.clone(),
        source: routine.source_workspace.clone(),
        origin: routine.origin.as_str(),
        action: "error",
        reason: Some(reason),
        slot: None,
        run_id: None,
    }
}

fn action(routine: &LoadedRoutine, action: &'static str) -> RoutineSweepReport {
    RoutineSweepReport {
        routine: routine.definition.name.clone(),
        source: routine.source_workspace.clone(),
        origin: routine.origin.as_str(),
        action,
        reason: None,
        slot: None,
        run_id: None,
    }
}

fn parse_rfc3339(raw: &str) -> Result<DateTime<Utc>, OrbitError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| OrbitError::Store(format!("invalid stored timestamp '{raw}': {error}")))
}
