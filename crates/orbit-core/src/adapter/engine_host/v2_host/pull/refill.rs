//! `pull_refill`: one iteration of a follower's pull drain [ORB-13625].
//!
//! Each call carries every earlier admission forward — retry, bind, launch,
//! settle — and, while the window is open and the owner would admit this
//! executor, tops the free slots up with new pull requests. It never fails the
//! drain for an owner that is unreachable or refusing: that is reported in the
//! output and the next iteration tries again, because the settlements of work
//! already running must keep flowing whatever the owner currently says about
//! new work.
//!
//! The drain outlives its window. `unsettled` counts admissions that still hold
//! a slot, and the job loop runs until the window has closed *and* that count
//! is zero, so a leaf that finishes after the window still has its handoff
//! delivered by the drain that admitted it.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::{
    AdmissionRequest, AdmissionRunContext, AdmissionShipContract,
    DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA, PullDestination,
};
use serde_json::{Value, json};

use super::adapters::{LeafPullLauncher, RoutedPullPeer};
use super::drain::PullDrain;
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

    // Stop admitting while a host shutdown is pending: anything started now
    // would be killed by it [ORB-12968]. Settlement still runs.
    let host_shutdown = runtime.scheduled_host_shutdown();
    let mut admitted = 0;
    let mut refusal = None;
    let mut error = None;
    let admitting = !window_expired && host_shutdown.is_none();
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
                match drain.refill(&destination, &template, ceiling) {
                    Ok(count) => admitted = count,
                    Err(failure) => error = Some(failure.to_string()),
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
    // Refill already reconciled when it ran; otherwise reconcile here so
    // settlements reach the owner even while it refuses new work.
    if (!admitting || refusal.is_some() || (error.is_some() && admitted == 0))
        && let Err(failure) = drain.reconcile_pending(&destination)
    {
        error.get_or_insert(failure.to_string());
    }
    let unsettled = drain
        .unsettled(&destination)
        .map_err(|failure| failed(failure.to_string()))?;
    let done = window_expired && unsettled == 0;
    let sleep_seconds = if admitted > 0 {
        0
    } else if unsettled > 0 || error.is_some() {
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
        "refusal": refusal,
        "error": error,
        "host_shutdown": host_shutdown.map(|shutdown| shutdown.describe()),
        "done": done,
        "wait": !done && sleep_seconds > 0,
        "sleep_seconds": sleep_seconds,
    }))
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
