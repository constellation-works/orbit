//! `pull_refill`: one iteration of a follower's pull drain [ORB-13625].
//!
//! Each call carries every earlier admission forward — retry, bind, launch,
//! settle — and, while the window is open and the owner would admit this
//! executor, tops the free slots up with new pull requests. It never fails the
//! drain for an owner that is unreachable or refusing, nor for a local store
//! read that fails: that is reported in the output and the next iteration tries
//! again, because the settlements of work already running must keep flowing
//! whatever the owner currently says about new work.
//!
//! Each request declares the crews this window can run [ORB-13941]: the
//! provider preflight taken on the window's first pass, minus every crew a
//! claimed leaf has since found unusable. The owner skips a task whose crew is
//! not among them, so a follower never burns a claim it cannot run.
//!
//! The drain outlives its window. `unsettled` counts admissions that still hold
//! a slot, and the job loop runs until the window has closed *and* that count
//! is zero, so a leaf that finishes after the window still has its handoff
//! delivered: by the leaf's own bound worker as it ends, by any later settle
//! pass, or by this drain's next iteration, whichever gets there first.
//!
//! Sustained host resource pressure holds requests the same way a pending
//! host shutdown does [ORB-13901]: the pass requests nothing, keeps settling,
//! records the throttle on the drain's last pass and polls for recovery.
//!
//! A graceful `orbit run cancel` puts the drain into its cancelling mode:
//! each pass then requests nothing, releases every admission that never
//! launched back to the owner's backlog, and waits for the launched leaves to
//! finish and settle. The pass that finds nothing left unsettled ends the
//! drain `cancelled`.

use std::cell::RefCell;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::{
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, LocalPullAdmission, PullDestination,
};
use orbit_types::workflow::{
    CrewExclusion, CrewExclusionSource, DrainAdmissionPass, DrainCancelRequest, PullCrewPreflight,
    ResourceThrottle,
};
use serde_json::{Value, json};

use super::super::cli_executor::resolve_cli_executor;
use super::adapters::{LeafPullLauncher, RoutedPullPeer};
use super::drain::{CONSECUTIVE_FAILURE_BREAKER, PullDrain};
use crate::OrbitRuntime;
use crate::application::distributed::{PullCrewWindow, owner_binary_version};

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
    };
    // A launched leaf whose worker died is reconciled first, so this pass
    // records and delivers its failure rather than waiting on it.
    runtime.reconcile_orphaned_claimed_leaves(&destination);

    let cancel = runtime
        .read_run_state(&run_id)
        .ok()
        .flatten()
        .and_then(|state| state.drain_cancel);
    if let Some(cancel) = cancel {
        return Ok(cancelling_pass(
            runtime,
            &drain,
            &run_id,
            &destination,
            &cancel,
            poll,
        ));
    }

    // Stop admitting while a host shutdown is pending: anything started now
    // would be killed by it [ORB-12968]. Settlement still runs.
    let host_shutdown = runtime.scheduled_host_shutdown();
    // Sustained host pressure holds new requests too [ORB-13901]; unknown
    // telemetry admits.
    let resource = runtime.resource_admission();
    let mut admitted = 0;
    let mut refusal = None;
    let mut error: Option<String> = None;
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
    let mut admitting = !window_expired
        && host_shutdown.is_none()
        && resource.throttle.is_none()
        && !breaker_open
        && consecutive_failures.is_some();
    // Whether `refill` ran, and so already reconciled this pass.
    let mut refilled = false;
    if admitting {
        match probe(runtime, &transport, &destination) {
            Ok(ProbeVerdict {
                ship: Some(ship),
                refusal: None,
            }) => {
                let template = || {
                    let window = crew_window(runtime, &run_id)?;
                    let capability = (!window.runs_nothing()).then(|| window.capability());
                    *crews.borrow_mut() = Some(window);
                    Ok(capability.map(|capability| AdmissionRequest {
                        request_id: String::new(),
                        caller_version: owner_binary_version().to_string(),
                        caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                        caller_review_policy: runtime.local_review_policy_label(),
                        run_context: AdmissionRunContext {
                            run_id: run_id.clone(),
                            job_name: PULL_DRAIN_JOB_NAME.to_string(),
                            machine_name: None,
                        },
                        ship: ship.clone(),
                        crews: Some(capability),
                    }))
                };
                let ceiling = usize::try_from(ceiling).unwrap_or(usize::MAX);
                let pass = drain.refill_pass(&destination, &template, ceiling);
                refilled = true;
                admitted = pass.admitted;
                if let Some(failure) = pass.error {
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
    let done = window_expired && !unsettled_holding;
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
    let refusal = refusal
        .or_else(|| {
            breaker_open.then(|| {
                format!(
                    "circuit_open: the last {consecutive_failures} claims this drain admitted all \
                     settled as failures; inspect them and start a new drain once the cause is \
                     fixed"
                )
            })
        })
        .or_else(|| {
            runs_nothing.then(|| {
                "no_runnable_crew: every configured crew is excluded on this host for this \
                 window; see `crews.excluded`, fix the providers, and start a new drain"
                    .to_string()
            })
        });
    record_pass(runtime, &run_id, resource.throttle.clone());
    Ok(json!({
        "admitted": admitted,
        "unsettled": unsettled,
        "admitting": admitting,
        "refusal": refusal,
        "crews": crews,
        "consecutive_failures": consecutive_failures,
        "reclaimed_build_bytes": reclaimed_build_bytes,
        "error": error,
        "host_shutdown": host_shutdown.map(|shutdown| shutdown.describe()),
        "resource_throttle": resource.throttle,
        "resource_telemetry_unknown": resource.unknown,
        "cancelling": false,
        "done": done,
        "wait": !done && sleep_seconds > 0,
        "sleep_seconds": sleep_seconds,
    }))
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
) -> Value {
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
    json!({
        "admitted": 0,
        "unsettled": unsettled,
        "admitting": false,
        "refusal": Value::Null,
        "consecutive_failures": 0,
        "reclaimed_build_bytes": reclaimed_build_bytes,
        "error": error,
        "host_shutdown": Value::Null,
        "resource_throttle": Value::Null,
        "resource_telemetry_unknown": [],
        "cancelling": true,
        "waiting_leaves": waiting_leaves,
        "done": done,
        "wait": !done,
        "sleep_seconds": if done { 0 } else { poll },
    })
}

/// Record this pass on the drain's own state, so readiness and `orbit run
/// show` report a throttle the drain is holding. A pull drain queues nothing
/// locally: the owner orders the backlog. Best effort, like the local drain's.
fn record_pass(runtime: &OrbitRuntime, run_id: &str, resource_throttle: Option<ResourceThrottle>) {
    let mut pass = Some(DrainAdmissionPass {
        recorded_at: chrono::Utc::now(),
        queued: 0,
        deferred: Vec::new(),
        excluded: Vec::new(),
        excluded_total: 0,
        resource_throttle,
    });
    if let Err(failure) = runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.drain_last_pass = pass.take();
            Ok(())
        })
    {
        tracing::warn!(
            target: "orbit.core.pull",
            run_id,
            %failure,
            "pull drain could not record its pass; readiness will not report its throttle",
        );
    }
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
/// leaf would launch it. Cheap: no provider process is started. No shipped
/// provider declares a side-effect-free authentication probe, so an
/// unauthenticated CLI passes here and is caught by its first claimed leaf,
/// whose typed provider failure excludes the crew for the rest of the window.
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
/// protocol schema and this host's review policy makes the owner report the
/// first refusal admission would raise, so a mismatch stops new requests
/// before any is persisted.
fn probe(
    runtime: &OrbitRuntime,
    transport: &std::sync::Arc<dyn orbit_tools::DrainOwnerTransport>,
    destination: &PullDestination,
) -> Result<ProbeVerdict, OrbitError> {
    let report = transport.call(
        &destination.selector,
        "orbit.drain.probe",
        json!({
            "caller_version": owner_binary_version(),
            "caller_schema": DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
            "caller_review_policy": runtime.local_review_policy_label(),
        }),
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
    Ok(ProbeVerdict {
        ship,
        refusal: None,
    })
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
