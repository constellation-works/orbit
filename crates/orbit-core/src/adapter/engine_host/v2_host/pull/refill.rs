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
//! The drain outlives its window. `unsettled` counts admissions that still hold
//! a slot, and the job loop runs until the window has closed *and* that count
//! is zero, so a leaf that finishes after the window still has its handoff
//! delivered: by the leaf's own bound worker as it ends, by any later settle
//! pass, or by this drain's next iteration, whichever gets there first.
//!
//! A graceful `orbit run cancel` puts the drain into its cancelling mode:
//! each pass then requests nothing, releases every admission that never
//! launched back to the owner's backlog, and waits for the launched leaves to
//! finish and settle. The pass that finds nothing left unsettled ends the
//! drain `cancelled`.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::{
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, LocalPullAdmission, PullDestination,
};
use orbit_types::workflow::DrainCancelRequest;
use serde_json::{Value, json};

use super::adapters::{LeafPullLauncher, RoutedPullPeer};
use super::drain::{CONSECUTIVE_FAILURE_BREAKER, PullDrain};
use crate::OrbitRuntime;
use crate::application::distributed::owner_binary_version;

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
    let mut admitted = 0;
    let mut refusal = None;
    let mut error: Option<String> = None;
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
                let template = AdmissionRequest {
                    request_id: String::new(),
                    caller_version: owner_binary_version().to_string(),
                    caller_schema: DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
                    caller_review_policy: runtime.local_review_policy_label(),
                    run_context: AdmissionRunContext {
                        run_id: run_id.clone(),
                        job_name: PULL_DRAIN_JOB_NAME.to_string(),
                        machine_name: None,
                    },
                    ship,
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
    } else if unsettled_holding || error.is_some() {
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
    Ok(json!({
        "admitted": admitted,
        "unsettled": unsettled,
        "admitting": admitting,
        "refusal": refusal.or_else(|| breaker_open.then(|| format!(
            "circuit_open: the last {consecutive_failures} claims this drain admitted all settled as \
             failures; inspect them and start a new drain once the cause is fixed"
        ))),
        "consecutive_failures": consecutive_failures,
        "reclaimed_build_bytes": reclaimed_build_bytes,
        "error": error,
        "host_shutdown": host_shutdown.map(|shutdown| shutdown.describe()),
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
        "cancelling": true,
        "waiting_leaves": waiting_leaves,
        "done": done,
        "wait": !done,
        "sleep_seconds": if done { 0 } else { poll },
    })
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
