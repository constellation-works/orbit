//! A pass of a gracefully cancelled pull drain [ORB-13663].

use orbit_common::OrbitError;
use orbit_store::contracts::{LocalPullAdmission, PullDestination};
use orbit_types::workflow::DrainCancelRequest;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::distributed::OwnerAnswer;
use crate::application::pull_drain::drain::PullDrain;

/// One pass of a gracefully cancelled drain: release what never launched,
/// settle what ended, wait for live leaves, and end the drain `cancelled`
/// once nothing it carries is unsettled.
pub(super) fn cancelling_pass(
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
    let health = runtime.record_pull_pass(
        run_id,
        None,
        None,
        error.as_deref(),
        None,
        OwnerAnswer::None,
        None,
    )?;
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
