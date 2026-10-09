//! `pull_refill`: one iteration of a follower's pull drain [ORB-13625].
//!
//! Each call carries every earlier admission forward — retry, bind, launch,
//! settle — and, while the window is open and the owner would admit this
//! executor, tops the free slots up with new pull requests. Three consecutive
//! failed passes latch a durable degraded warning and stop new admissions.
//! Protocol skew fails immediately with its typed code and preserves the
//! durable settlement outbox. Other settlement retries keep flowing. If run state cannot be read or recorded,
//! the activity fails visibly rather than retrying without health evidence.
//!
//! Each request declares the crews this window can run [ORB-13941]: the
//! provider preflight taken on the window's first pass, minus every crew a
//! claimed leaf has since found unusable, and within the drain's
//! `--allow-crew` restriction when it has one [ORB-14174]. The owner skips a
//! task whose crew is not among them, so a follower never burns a claim it
//! cannot run. The owner's before-PR reviewer is checked against the window
//! without that restriction. Each request also declares this host's OS, and
//! the owner skips a task whose `os:` tags that OS does not satisfy.
//!
//! A drain submitted without a window (`for_seconds` zero) is authorized for
//! one admission pass [ORB-14174]. Its window is expired from the start, so
//! the pass is not read off the window: the first pass that finds no stop or
//! cancel takes it, recording it in run state before probing or requesting,
//! and requests up to the free slots. Every later pass — the same run's,
//! a retry's, or a resumed run's — finds it taken and only settles. A pass
//! held by a throttle, a shutdown or an owner refusal is still that pass.
//! A timed window that has expired never gains one.
//!
//! A stop or cancel recorded while a pass is requesting ends it before the
//! next request.
//!
//! The drain outlives its window. `unsettled` counts admissions that still hold
//! a slot, and the job loop runs until the window has closed *and* that count
//! is zero, so a leaf that finishes after the window still has its handoff
//! delivered: by the leaf's own bound worker as it ends, by any later settle
//! pass, or by this drain's next iteration, whichever gets there first.
//!
//! A settlement the owner refuses while it still holds the claim holds new
//! requests too [ORB-13979]: the owner answered, and will answer the same
//! until an operator changes it. The refusal is recorded on the admission,
//! logged once and listed by `orbit run show`; delivery backs off (doubling to
//! at most 15 minutes) instead of asking on every pass, and is not a failed
//! pass. Requests resume on the pass after the owner accepts it.
//!
//! Sustained host resource pressure holds requests the same way a pending
//! host shutdown does [ORB-13901]: the pass requests nothing, keeps settling,
//! records the throttle on the drain's last pass and polls for recovery.
//!
//! A graceful `orbit run cancel` puts the drain into its cancelling mode:
//! each pass then requests nothing, releases every admission that never
//! launched back to the owner's backlog, and waits for the launched leaves to
//! finish and settle. The pass that finds nothing left unsettled ends the
//! drain `cancelled`. Cancellation is read again after orphaned leaves are
//! reconciled, so a request recorded during that work is observed before any
//! probe, pull request, or leaf launch in the same pass. If that state cannot
//! be read, the entire pass waits and fails before probing or advancing
//! pending requests: neither new work nor an earlier unanswered request may
//! be admitted without knowing whether the drain is cancelling.

use std::cell::RefCell;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::{
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, LocalPullAdmission, PullDestination,
};
use orbit_types::workflow::{
    CrewExclusion, CrewExclusionSource, DrainCancelRequest, PullCrewPreflight,
};
use serde_json::{Value, json};

use super::super::cli_executor::resolve_cli_executor;
use super::adapters::{LeafPullLauncher, RoutedPullPeer};
use super::drain::{CONSECUTIVE_FAILURE_BREAKER, PullDrain, RefusedDelivery};
use crate::OrbitRuntime;
use crate::application::distributed::{
    OwnerAnswer, PullCrewWindow, RefusedPullSettlement, owner_binary_version,
};

/// The job that runs this action. Recorded in each request's run context, so
/// the owner's claim names the drain that holds it.
pub(crate) const PULL_DRAIN_JOB_NAME: &str = "workspace_pull_pipeline";

const DEFAULT_MAX_ACTIVE_LEAF_RUNS: u64 = 5;
const DEFAULT_POLL_SLEEP_SECONDS: u64 = 30;
const DEFAULT_IDLE_SLEEP_SECONDS: u64 = 60;

/// What the owner's probe said about admitting this executor now.
struct ProbeVerdict {
    ship: Option<AdmissionShipContract>,
    refusal: Option<String>,
}

pub(crate) fn pull_refill(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let failed = |message: String| DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    };
    let run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| failed("the executing drain run id was not supplied".into()))?
        .to_string();
    let destination: PullDestination = input
        .get("destination")
        .cloned()
        .ok_or_else(|| failed("`destination` is required".into()))
        .and_then(|value| {
            serde_json::from_value(value).map_err(|error| failed(format!("`destination`: {error}")))
        })?;
    let window_expired = bool_input(input, "window_expired");
    let single_pass = single_pass_input(input);
    let ceiling = u64_input(input, "max_active_leaf_runs", DEFAULT_MAX_ACTIVE_LEAF_RUNS);
    let poll = u64_input(input, "poll_sleep_seconds", DEFAULT_POLL_SLEEP_SECONDS);
    let idle = u64_input(input, "idle_sleep_seconds", DEFAULT_IDLE_SLEEP_SECONDS);

    let transport = runtime.drain_owner_transport().cloned().ok_or_else(|| {
        failed(format!(
            "no route to owner '{}': this runtime was composed without a federated owner \
                 transport, and a pull drain never falls back to its own store",
            destination.owner_machine_id
        ))
    })?;
    let peer = RoutedPullPeer {
        transport: transport.clone(),
    };
    let launcher = LeafPullLauncher { runtime };
    let drain = PullDrain {
        jobs: runtime.stores().jobs(),
        peer: &peer,
        launcher: &launcher,
        refused_delivery: RefusedDelivery::WhenDue,
    };
    let read_pass_state = || {
        runtime.read_run_state(&run_id).map_err(|error| {
            failed(format!(
                "pull drain could not read cancellation and pass health: {error}"
            ))
        })
    };
    let state = read_pass_state()?;
    let degraded = state
        .as_ref()
        .and_then(|state| state.drain_last_pass.as_ref())
        .is_some_and(|pass| pass.degraded);
    if let Some(pass) = state
        .as_ref()
        .and_then(|state| state.drain_last_pass.as_ref())
        && pass.degraded
        && (pass.last_pass_error_code.as_deref() == Some("protocol_skew")
            || pass
                .last_pass_error
                .as_deref()
                .is_some_and(|message| message.starts_with("protocol_mismatch:")))
    {
        return Err(DispatchError::ProtocolSkew(
            pass.last_pass_error
                .clone()
                .unwrap_or_else(|| "pull request schema mismatch".into()),
        ));
    }
    // A launched leaf whose worker died is reconciled first, so this pass
    // records and delivers its failure rather than waiting on it. Cancellation
    // is read again afterwards: an operator can record it while reconciliation
    // runs, and the value from before that work must not admit new work.
    runtime.reconcile_orphaned_claimed_leaves(&destination);
    let cancel = read_pass_state()?.and_then(|state| state.drain_cancel);
    if let Some(cancel) = cancel {
        return cancelling_pass(runtime, &drain, &run_id, &destination, &cancel, poll)
            .map_err(|error| failed(error.to_string()));
    }

    // A windowless drain's only admission pass is this one if nothing has
    // taken it yet; a timed window is open until it expires.
    let window_open = if single_pass {
        runtime.take_pull_single_pass(&run_id).map_err(|error| {
            failed(format!(
                "pull drain could not record its single admission pass: {error}"
            ))
        })?
    } else {
        !window_expired
    };
    // Stop admitting while a host shutdown is pending: anything started now
    // would be killed by it [ORB-12968]. Settlement still runs.
    let host_shutdown = runtime.scheduled_host_shutdown();
    // Sustained host pressure holds new requests too [ORB-13901]; unknown
    // telemetry admits.
    runtime.reclaim_worktrees_on_admission();
    let resource = runtime.resource_admission();
    let mut admitted = 0;
    let mut refusal = None;
    let mut error: Option<String> = None;
    // What the owner said to the requests this pass sent [ORB-14475].
    let mut idle_receipt = None;
    // What this window can run: its preflight, minus every crew a leaf has
    // since found unusable [ORB-13941]. Read by the refill after it has
    // reconciled, so a leaf this pass settles already counts.
    let crews: RefCell<Option<PullCrewWindow>> = RefCell::new(None);
    // An already open breaker skips the probe; `refill` rechecks it after
    // reconciling, since settling a newly failed leaf can open it mid-pass.
    // A streak that cannot be read admits nothing: the drain reports it and
    // tries again next iteration.
    let mut consecutive_failures = match drain.consecutive_failed_settlements(&destination, &run_id)
    {
        Ok(count) => Some(count),
        Err(failure) => {
            error = Some(failure.to_string());
            None
        }
    };
    let breaker_open =
        consecutive_failures.is_some_and(|count| count >= CONSECUTIVE_FAILURE_BREAKER);
    // A settlement the owner refused holds new requests until it is
    // accepted; this pass still delivers it once its backoff is due.
    let settlement_held = match refused_settlements(&drain, &destination) {
        Ok(refused) => !refused.is_empty(),
        Err(failure) => {
            error.get_or_insert(failure.to_string());
            true
        }
    };
    let mut admitting = !degraded
        && window_open
        && host_shutdown.is_none()
        && resource.throttle.is_none()
        && !breaker_open
        && !settlement_held
        && consecutive_failures.is_some();
    // Whether `refill` ran, and so already reconciled this pass.
    let mut refilled = false;
    if admitting && let Err(failure) = runtime.recover_pull_auth_exclusions(&run_id) {
        error.get_or_insert(failure.to_string());
        admitting = false;
    }
    if admitting {
        match probe(runtime, &run_id, &transport, &destination) {
            Ok(ProbeVerdict {
                ship: Some(ship),
                refusal: None,
            }) => {
                let template = || {
                    let caller_before_pr = captured_before_pr(runtime, &run_id)?;
                    let window = crew_window(runtime, &run_id)?;
                    let capability = (!window.runs_nothing()).then(|| window.capability());
                    *crews.borrow_mut() = Some(window);
                    Ok(capability.map(|capability| AdmissionRequest {
                        request_id: String::new(),
                        caller_version: owner_binary_version().to_string(),
                        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                        caller_fingerprint: Some(
                            orbit_store::contracts::distributed_drain_protocol_fingerprint()
                                .to_string(),
                        ),
                        caller_before_pr,
                        // The claimed PR leaf runs the before-PR gate the
                        // ship contract captures [ORB-13908].
                        review_gate: true,
                        run_context: AdmissionRunContext {
                            run_id: run_id.clone(),
                            job_name: PULL_DRAIN_JOB_NAME.to_string(),
                            machine_name: runtime
                                .automation_execution_location()
                                .and_then(|location| location.machine_name.clone()),
                        },
                        ship: ship.clone(),
                        crews: Some(capability),
                        os: runtime.host_os(),
                    }))
                };
                // A stop or cancel recorded mid-pass ends it before the next
                // request; one that cannot be read ends it too.
                let still_admitting = || {
                    Ok(runtime.read_run_state(&run_id)?.is_none_or(|state| {
                        state.drain_admissions_stop.is_none() && state.drain_cancel.is_none()
                    }))
                };
                let ceiling = usize::try_from(ceiling).unwrap_or(usize::MAX);
                let pass = drain.refill_pass(&destination, &template, &still_admitting, ceiling);
                refilled = true;
                admitted = pass.admitted;
                idle_receipt = pass.answer;
                if let Some(failure) = pass.error {
                    if let OrbitError::ProtocolSkew(message) = failure {
                        return Err(protocol_skew_failure(
                            runtime,
                            action,
                            &run_id,
                            resource.throttle.clone(),
                            message,
                        ));
                    }
                    error = Some(failure.to_string());
                }
            }
            Ok(verdict) => {
                refusal = Some(
                    verdict
                        .refusal
                        .unwrap_or_else(|| "owner probe returned no ship contract".into()),
                );
            }
            Err(OrbitError::ProtocolSkew(message)) => {
                return Err(protocol_skew_failure(
                    runtime,
                    action,
                    &run_id,
                    resource.throttle.clone(),
                    message,
                ));
            }
            Err(failure) => error = Some(failure.to_string()),
        }
    }
    // Refill reconciles first, so when it ran it has already tried every
    // pending admission, failed or not. Otherwise reconcile here so
    // settlements reach the owner even while it refuses new work.
    if !refilled && let Err(failure) = drain.reconcile_pending(&destination) {
        error.get_or_insert(failure.to_string());
    }
    // Settled leaves give back their build output here, not on an external
    // GC schedule: a follower's disk would otherwise grow by a leaf's
    // `target/` per claim [ORB-13920].
    let reclaimed_build_bytes = runtime.reclaim_settled_leaf_build_output();
    // Count after this pass's settlements, so a breaker that opened during it
    // is reported now rather than on the next poll.
    match drain.consecutive_failed_settlements(&destination, &run_id) {
        Ok(count) => consecutive_failures = Some(count),
        Err(failure) => {
            error.get_or_insert(failure.to_string());
        }
    }
    let consecutive_failures = consecutive_failures.unwrap_or(0);
    let breaker_open = consecutive_failures >= CONSECUTIVE_FAILURE_BREAKER;
    admitting &= !breaker_open;
    // The window as this pass left it, read again when the refill did not
    // run or stopped before building a request.
    let crews = match crews.into_inner() {
        Some(window) => Some(window),
        None => match crew_window(runtime, &run_id) {
            Ok(window) => Some(window),
            Err(failure) => {
                error.get_or_insert(failure.to_string());
                None
            }
        },
    };
    let runs_nothing = crews.as_ref().is_some_and(PullCrewWindow::runs_nothing);
    admitting &= !runs_nothing;
    // As this pass left them: one it just delivered no longer holds requests.
    let refused = refused_settlements(&drain, &destination).unwrap_or_else(|failure| {
        error.get_or_insert(failure.to_string());
        Vec::new()
    });
    admitting &= refused.is_empty();
    // An unreadable count is not zero: the drain must not finish while it
    // cannot tell whether a settlement is still owed.
    let unsettled = match drain.unsettled(&destination) {
        Ok(count) => Some(count),
        Err(failure) => {
            error.get_or_insert(failure.to_string());
            None
        }
    };
    let unsettled_holding = unsettled.is_none_or(|count| count > 0);
    // A windowless drain admits on no later pass, whether or not this one did.
    let done = (single_pass || window_expired) && !unsettled_holding;
    let sleep_seconds = if admitted > 0 {
        0
    } else if unsettled_holding || error.is_some() || resource.throttle.is_some() {
        poll
    } else {
        idle
    };
    if let Some(message) = error.as_deref() {
        tracing::warn!(
            target: "orbit.core.pull",
            owner = %destination.owner_machine_id,
            selector = %destination.selector,
            %message,
            "pull drain pass did not complete; retrying next iteration",
        );
    }
    let host_suppressed = crews
        .as_ref()
        .and_then(|window| window.host_suppressed.as_deref());
    let no_runnable_crew = if let Some(reason) = host_suppressed {
        format!(
            "host_suppressed: this drain claims no more work on this host for its window \
             because {reason}; see `crews.host_suppressed`, fix the host, and start a new drain"
        )
    } else if crews
        .as_ref()
        .is_some_and(|window| window.allowed.is_some())
    {
        "no_runnable_crew: no crew this drain's --allow-crew permits can run on this host for \
         this window; see `crews.allowed` and `crews.excluded`, then start a new drain with \
         crews that run here"
            .to_string()
    } else {
        "no_runnable_crew: every configured crew is excluded on this host for this window; see \
         `crews.excluded`, fix the providers, and start a new drain"
            .to_string()
    };
    let refusal = refusal
        .or_else(|| {
            (!refused.is_empty()).then(|| {
                format!(
                    "settlement_refused: the owner refused {} recorded settlement(s) while \
                     holding their claims, so no new claim is requested; `orbit run show \
                     {run_id}` names the reason and the remedy",
                    refused.len()
                )
            })
        })
        .or_else(|| {
            breaker_open.then(|| {
                format!(
                    "circuit_open: the last {consecutive_failures} claims this drain admitted all \
                     settled as failures; inspect them and start a new drain once the cause is \
                     fixed"
                )
            })
        })
        .or_else(|| runs_nothing.then_some(no_runnable_crew));
    let owner_answer = match idle_receipt.as_deref() {
        Some(receipt) => OwnerAnswer::Idle(receipt),
        None if admitted > 0 => OwnerAnswer::Claimed,
        None => OwnerAnswer::None,
    };
    let health = runtime
        .record_pull_pass(
            &run_id,
            resource.throttle.clone(),
            error.as_deref(),
            None,
            owner_answer,
        )
        .map_err(|error| failed(format!("pull drain could not record pass health: {error}")))?;
    admitting &= !health.degraded;
    let refusal =
        refusal.or_else(|| {
            health.degraded.then(|| format!(
            "pass_failures: drain degraded after {} consecutive failed passes; fix the cause \
             run `orbit run auto --stop`, and start a new drain once this one ends; settlement retries continue",
            health.consecutive_pass_failures
        ))
        });
    Ok(json!({
        "admitted": admitted,
        "unsettled": unsettled,
        "admitting": admitting,
        "refusal": refusal,
        "settlement_refused": refused.len(),
        "crews": crews,
        "consecutive_failures": consecutive_failures,
        "reclaimed_build_bytes": reclaimed_build_bytes,
        "error": error,
        "last_pass_error": health.last_pass_error,
        "consecutive_pass_failures": health.consecutive_pass_failures,
        "degraded": health.degraded,
        "host_shutdown": host_shutdown.map(|shutdown| shutdown.describe()),
        "resource_throttle": resource.throttle,
        "resource_telemetry_unknown": resource.unknown,
        "cancelling": false,
        "done": done,
        "wait": !done && sleep_seconds > 0,
        "sleep_seconds": sleep_seconds,
    }))
}

/// Preserve the cause before the engine terminalizes the drain. Its outbox
/// remains available to leaf workers and later settlement-only passes.
fn protocol_skew_failure(
    runtime: &OrbitRuntime,
    action: &str,
    run_id: &str,
    throttle: Option<orbit_types::workflow::ResourceThrottle>,
    message: String,
) -> DispatchError {
    match runtime.record_pull_pass(
        run_id,
        throttle,
        Some(&format!("protocol_skew: {message}")),
        Some("protocol_skew"),
        OwnerAnswer::None,
    ) {
        Ok(_) => DispatchError::ProtocolSkew(message),
        Err(error) => DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("pull drain could not record pass health: {error}"),
        },
    }
}

/// The settlements for `destination` its owner refused while still holding
/// their claims [ORB-13979].
fn refused_settlements(
    drain: &PullDrain<'_>,
    destination: &PullDestination,
) -> Result<Vec<RefusedPullSettlement>, OrbitError> {
    Ok(drain
        .jobs
        .unsettled_local_pull_admissions()?
        .iter()
        .filter(|record| record.destination == *destination)
        .filter_map(RefusedPullSettlement::of)
        .collect())
}

/// One pass of a gracefully cancelled drain: release what never launched,
/// settle what ended, wait for live leaves, and end the drain `cancelled`
/// once nothing it carries is unsettled.
fn cancelling_pass(
    runtime: &OrbitRuntime,
    drain: &PullDrain<'_>,
    run_id: &str,
    destination: &PullDestination,
    cancel: &DrainCancelRequest,
    poll: u64,
) -> Result<Value, OrbitError> {
    let cause = match cancel.reason.as_deref() {
        Some(reason) => format!("the drain was cancelled by {}: {reason}", cancel.actor),
        None => format!("the drain was cancelled by {}", cancel.actor),
    };
    // Only what this drain carries: another live drain's work is its own.
    let mut error = None;
    let carried = runtime
        .pull_drain_admissions(run_id)
        .map(|records| {
            records
                .into_iter()
                .map(|record| (record.destination, record.request.request_id))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|failure| {
            error = Some(failure.to_string());
            Vec::new()
        });
    let carries = |record: &LocalPullAdmission| {
        carried
            .iter()
            .any(|(to, id)| *to == record.destination && *id == record.request.request_id)
    };
    if let Err(failure) = drain.release_pending(&carries, &cause) {
        error.get_or_insert(failure.to_string());
    }
    let reclaimed_build_bytes = runtime.reclaim_settled_leaf_build_output();
    let unsettled = match runtime.pull_drain_admissions(run_id) {
        Ok(records) => Some(records.len()),
        Err(failure) => {
            error.get_or_insert(failure.to_string());
            None
        }
    };
    let waiting_leaves = match runtime.pull_drain_claimed_leaves(run_id) {
        Ok(leaves) => leaves,
        Err(failure) => {
            error.get_or_insert(failure.to_string());
            Vec::new()
        }
    };
    let mut done = unsettled == Some(0);
    if done && let Err(failure) = runtime.complete_graceful_drain_cancel(run_id) {
        // The drain stays running and the next pass tries again.
        error.get_or_insert(failure.to_string());
        done = false;
    }
    if let Some(message) = error.as_deref() {
        tracing::warn!(
            target: "orbit.core.pull",
            selector = %destination.selector,
            %message,
            "cancelling pull drain pass did not complete; retrying next iteration",
        );
    }
    let health =
        runtime.record_pull_pass(run_id, None, error.as_deref(), None, OwnerAnswer::None)?;
    Ok(json!({
        "admitted": 0,
        "unsettled": unsettled,
        "admitting": false,
        "refusal": Value::Null,
        "consecutive_failures": 0,
        "reclaimed_build_bytes": reclaimed_build_bytes,
        "error": error,
        "last_pass_error": health.last_pass_error,
        "consecutive_pass_failures": health.consecutive_pass_failures,
        "degraded": health.degraded,
        "host_shutdown": Value::Null,
        "resource_throttle": Value::Null,
        "resource_telemetry_unknown": [],
        "cancelling": true,
        "waiting_leaves": waiting_leaves,
        "done": done,
        "wait": !done,
        "sleep_seconds": if done { 0 } else { poll },
    }))
}

/// The drain's crew window, taking and persisting its preflight on the first
/// pass of the window. A preflight that cannot be persisted is still used for
/// this pass, and taken again on the next.
fn crew_window(runtime: &OrbitRuntime, run_id: &str) -> Result<PullCrewWindow, OrbitError> {
    let stored = runtime
        .read_run_state(run_id)?
        .and_then(|state| state.pull_crew_preflight);
    let preflight = match stored {
        Some(preflight) => preflight,
        None => {
            let preflight = crew_preflight(runtime);
            let mut pending = Some(preflight.clone());
            if let Err(failure) =
                runtime
                    .stores()
                    .jobs()
                    .update_run_state(run_id, &mut |_, state| {
                        if state.pull_crew_preflight.is_none() {
                            state.pull_crew_preflight = pending.take();
                        }
                        Ok(())
                    })
            {
                tracing::warn!(
                    target: "orbit.core.pull",
                    run_id,
                    %failure,
                    "pull drain could not record its crew preflight; the next pass takes it again",
                );
            }
            preflight
        }
    };
    runtime.crew_window_from(run_id, Some(preflight))
}

/// The window's provider preflight [ORB-13941]: every configured crew this
/// host could dispatch now, resolved the way dispatch resolves it — enabled,
/// its provider's executor resolvable, and that executor's CLI found where a
/// leaf would launch it. Cheap: no provider process is started. An
/// unauthenticated CLI passes here and is caught by its first claimed leaf.
/// Only auth-excluded providers with a declared probe may recover later.
fn crew_preflight(runtime: &OrbitRuntime) -> PullCrewPreflight {
    let registry = runtime.configured_crew_registry_projection();
    let mut runnable = Vec::new();
    let mut excluded = Vec::new();
    for crew in &registry.crews {
        let unusable = if crew.enabled {
            match resolve_cli_executor(runtime, &crew.provider) {
                Ok(executor) => runtime
                    .locate_provider_launcher(&executor.command)
                    .is_none()
                    .then(|| {
                        format!(
                            "provider `{}` CLI `{}` was not found on this host",
                            crew.provider, executor.command
                        )
                    }),
                Err(failure) => Some(format!("provider `{}`: {failure}", crew.provider)),
            }
        } else {
            Some(format!(
                "disabled here (`[crews.{}] enabled = false`)",
                crew.name
            ))
        };
        match unusable {
            None => runnable.push(crew.name.clone()),
            Some(reason) => excluded.push(CrewExclusion {
                crew: crew.name.clone(),
                source: CrewExclusionSource::Preflight,
                reason,
            }),
        }
    }
    PullCrewPreflight {
        checked_at: chrono::Utc::now(),
        runnable,
        default_crew: registry.default_crew,
        excluded,
    }
}

/// Ask the owner whether it would admit this executor now, and for the ship
/// contract a new request must carry. Declaring this binary's version, the
/// protocol schema and the drain's captured `review.before_pr` makes the
/// owner report the first refusal admission would raise, so a mismatch stops
/// new requests before any is persisted.
fn probe(
    runtime: &OrbitRuntime,
    run_id: &str,
    transport: &std::sync::Arc<dyn orbit_tools::DrainOwnerTransport>,
    destination: &PullDestination,
) -> Result<ProbeVerdict, OrbitError> {
    let report = crate::application::distributed::probe_pull_contract(
        transport.as_ref(),
        &destination.selector,
        captured_before_pr(runtime, run_id)?,
    )?;
    if report.get("owner_machine_id").and_then(Value::as_str)
        != Some(destination.owner_machine_id.as_str())
    {
        return Ok(ProbeVerdict {
            ship: None,
            refusal: Some(format!(
                "owner answered as {:?}, not the bound owner '{}'",
                report.get("owner_machine_id"),
                destination.owner_machine_id
            )),
        });
    }
    let admits = report.get("admits").and_then(Value::as_bool) == Some(true);
    if !admits {
        let refusal = report
            .get("refusal")
            .and_then(Value::as_str)
            .unwrap_or("refused");
        let diagnostics = report
            .get("diagnostics")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .unwrap_or_default();
        return Ok(ProbeVerdict {
            ship: None,
            refusal: Some(format!("{refusal}: {diagnostics}")),
        });
    }
    let ship = report
        .get("ship")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| OrbitError::Store(format!("owner probe ship contract: {error}")))?;
    if let Some(ship) = &ship
        && let Some(refusal) = reviewer_refusal(runtime, run_id, ship)?
    {
        return Ok(ProbeVerdict {
            ship: None,
            refusal: Some(refusal),
        });
    }
    Ok(ProbeVerdict {
        ship,
        refusal: None,
    })
}

/// Why this drain cannot run the before-PR review the owner's ship contract
/// captured [ORB-13908]: the crew is unset, does not resolve here, or the
/// drain's window cannot run it. Claiming anyway would only escalate the
/// task at its gate, so the drain requests nothing until the owner or this
/// host changes.
fn reviewer_refusal(
    runtime: &OrbitRuntime,
    run_id: &str,
    ship: &AdmissionShipContract,
) -> Result<Option<String>, OrbitError> {
    if let Some(refusal) = runtime.claimed_review_refusal(ship) {
        return Ok(Some(refusal));
    }
    let Some(crew) = ship
        .review
        .as_ref()
        .and_then(|review| review.crew.as_deref())
    else {
        return Ok(None);
    };
    // The window without its `--allow-crew` restriction: that selects the
    // claims' implementation crews, not the review each one owes.
    Ok(crew_window(runtime, run_id)?
        .reviewer_unrunnable_reason(crew)
        .map(|reason| {
            format!("before_pr_reviewer_unavailable: the owner's before-PR review {reason}")
        }))
}

/// Whether the drain was submitted without a window: `for_seconds` zero, as
/// the job forwards it. A pass given no `for_seconds` at all is a timed one.
fn single_pass_input(input: &Value) -> bool {
    match input.get("for_seconds") {
        Some(Value::Number(number)) => number.as_f64() == Some(0.0),
        Some(Value::String(text)) => {
            let text = text.trim();
            text.is_empty() || text.parse::<f64>().is_ok_and(|seconds| seconds == 0.0)
        }
        Some(Value::Null) => true,
        _ => false,
    }
}

/// A boolean templated into activity input, which renders as a string.
fn bool_input(input: &Value, key: &str) -> bool {
    match input.get(key) {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(text)) => text.trim() == "true",
        _ => false,
    }
}

fn u64_input(input: &Value, key: &str, default: u64) -> u64 {
    match input.get(key) {
        Some(Value::Number(number)) => number.as_u64().unwrap_or(default),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(default),
        _ => default,
    }
}

/// The `review.before_pr` the pull drain captured at submission
/// [ORB-13992]: turning it on later does not change what a running drain
/// declares. A drain submitted before the capture existed falls back to this
/// host's current setting.
fn captured_before_pr(runtime: &OrbitRuntime, run_id: &str) -> Result<bool, OrbitError> {
    Ok(
        crate::application::review::run_review_admission(runtime, run_id)?.map_or_else(
            || runtime.local_review_before_pr(),
            |admission| admission.gates_pr(),
        ),
    )
}
