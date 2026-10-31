//! Owner-clock lifetime maintenance; forge failures never prevent TTL expiry.
//! Each tick ends with a queue pass, so a hard lease waiting on a task that
//! settled or a reservation that ended is granted within one tick.

use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::fs::git::run_git;
use orbit_store::contracts::{LeaseRecord, LeaseReleaseParams, TaskReservationReleaseReason};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::json;

use super::super::audit::{emit_expired_lease_events, record_lease_audit_event};
use super::super::commands::{workspace_orbit_dir, workspace_task_reservation_id};
use crate::RuntimeKernel;

/// Expire leases and warn at 80% of the current TTL, release bound work
/// proven landed, then grant queued leases whose paths are free. Called only
/// by the owner's clock; dry runs do not call it.
/// Errors checking one landing leave that lease to its TTL and do not stop
/// checks for other leases. The shared tick deadline bounds new forge work.
pub fn sweep_lease_lifetimes(runtime: &RuntimeKernel, deadline: Instant) -> Result<(), OrbitError> {
    if runtime.coordination_write_owner().is_some() {
        return Ok(());
    }
    let orbit_dir = workspace_orbit_dir(runtime);
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let store = runtime.stores().task_reservations();
    let listed = store.sweep_lease_lifetimes(&orbit_dir, workspace_id.as_deref())?;
    emit_expired_lease_events(runtime, "orbit.clock.tick", &listed.expired_leases)?;
    for lease in listed.leases {
        if Instant::now() >= deadline {
            break;
        }
        let landing = match lease_landing(runtime, &lease) {
            Ok(Some(landing)) => landing,
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(lease_id = %lease.lease_id, %error, "lease landing check failed; TTL remains the backstop");
                continue;
            }
        };
        let released = store.release_lease(LeaseReleaseParams {
            workspace_orbit_dir: orbit_dir.clone(),
            workspace_id: workspace_id.clone(),
            lease_id: lease.lease_id,
            release_reason: TaskReservationReleaseReason::Landed,
            release_metadata_json: Some(landing.to_string()),
        })?;
        emit_expired_lease_events(runtime, "orbit.clock.tick", &released.expired_leases)?;
        if let Some(lease) = released.lease {
            record_lease_audit_event(
                runtime,
                "lease.released",
                "orbit.clock.tick",
                Some(&lease.lease_id),
                AuditEventStatus::Success,
                json!({"lease_id": lease.lease_id, "release_reason": "landed", "landing": landing}),
            )?;
        }
    }
    super::queue::grant_queued(runtime, "orbit.clock.tick")?;
    Ok(())
}

fn lease_landing(
    runtime: &RuntimeKernel,
    lease: &LeaseRecord,
) -> Result<Option<serde_json::Value>, OrbitError> {
    let (Some(binding), Some(base)) = (&lease.bound_branch, &lease.base_branch) else {
        return Ok(None);
    };
    let repo = &runtime.paths().repo_root;
    if let Some(pull) = runtime.task_pr_forge().lease_pull_request(repo, binding)? {
        return Ok((pull.merged && pull.base_branch == *base)
            .then(|| json!({"pull_request": pull.number, "base": base})));
    }
    // An explicit PR is never interpreted as a ref, even on a forge without
    // PR support. Fully qualified refs avoid ambiguous refs and option input.
    if binding.starts_with('#') {
        return Ok(None);
    }
    let branch = format!("refs/heads/{binding}");
    let base_ref = format!("refs/heads/{base}");
    let answer = run_git(repo, &["merge-base", "--is-ancestor", &branch, &base_ref])?;
    match answer.code {
        Some(0) => Ok(Some(json!({"branch": binding, "base": base}))),
        Some(1) => Ok(None),
        _ => Err(OrbitError::Execution(format!(
            "lease ancestry check failed: {}",
            answer.stderr.trim()
        ))),
    }
}
