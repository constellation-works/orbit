//! Operations on existing leases: renew, release, show, list, notices and
//! check.

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::optional_string_alias;
use orbit_store::contracts::{LeaseReleaseParams, LeaseRenewParams, TaskReservationReleaseReason};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use super::super::audit::{emit_expired_lease_events, record_lease_audit_event};
use super::super::commands::{
    parse_ttl_seconds, reservation_actor_label, workspace_orbit_dir, workspace_task_reservation_id,
};
use super::super::index::TaskLockIndex;
use super::admission::{LeaseTokenGrant, ensure_token_covers_lease};
use super::holders::{PathHolders, selector_covers};
use super::input::{parse_selectors, required_lease_id, validate_lease_id_form};
use super::queue;
use super::response::{ResponseBudget, lease_active, lease_json};
use crate::RuntimeKernel;

const RENEW_TOOL: &str = "orbit.lease.renew";
const RELEASE_TOOL: &str = "orbit.lease.release";
const SHOW_TOOL: &str = "orbit.lease.show";
const LIST_TOOL: &str = "orbit.lease.list";

/// Move an active lease's `expires_at` to `ttl_seconds` (reservation
/// bounds) from now.
pub fn renew(runtime: &RuntimeKernel, input: Value) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let lease_id = required_lease_id(&input)?;
    let ttl_seconds = parse_ttl_seconds(&input)?;
    let result = runtime
        .stores()
        .task_reservations()
        .renew_lease(LeaseRenewParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime)?,
            lease_id: lease_id.clone(),
            ttl_seconds,
        })?;
    emit_expired_lease_events(runtime, RENEW_TOOL, &result.expired_leases)?;
    if !result.expired_leases.is_empty() {
        queue::grant_queued(runtime, RENEW_TOOL)?;
    }
    let lease = result.lease.ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "lease `{lease_id}` is not active (unknown, released or expired); request a new lease"
        ))
    })?;
    record_lease_audit_event(
        runtime,
        "lease.renewed",
        RENEW_TOOL,
        Some(lease.lease_id.as_str()),
        AuditEventStatus::Success,
        json!({
            "lease_id": lease.lease_id,
            "ttl_seconds": ttl_seconds,
            "expires_at": lease.expires_at,
            "heartbeat_at": lease.heartbeat_at,
        }),
    )?;
    Ok(json!({"renewed": true, "lease": lease_json(&lease)}))
}

/// Renew only when the lease fits the calling token and its class policy.
/// An out-of-scope lease is left unchanged; operators may still renew it.
pub fn renew_with_token(
    runtime: &RuntimeKernel,
    input: Value,
    grant: &LeaseTokenGrant,
) -> Result<Value, OrbitError> {
    ensure_token_covers_lease(runtime, &input, grant)?;
    renew(runtime, input)
}

/// End an active lease. `released` is false when it was unknown or had
/// already ended.
pub fn release(
    runtime: &RuntimeKernel,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let lease_id = required_lease_id(&input)?;
    let released_by = reservation_actor_label(runtime, agent.as_deref(), model.as_deref())?;
    let result = runtime
        .stores()
        .task_reservations()
        .release_lease(LeaseReleaseParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime)?,
            lease_id,
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: Some(json!({"released_by": released_by}).to_string()),
        })?;
    emit_expired_lease_events(runtime, RELEASE_TOOL, &result.expired_leases)?;
    let Some(lease) = result.lease else {
        if !result.expired_leases.is_empty() {
            queue::grant_queued(runtime, RELEASE_TOOL)?;
        }
        return Ok(json!({"released": false, "lease": null}));
    };
    record_lease_audit_event(
        runtime,
        "lease.released",
        RELEASE_TOOL,
        Some(lease.lease_id.as_str()),
        AuditEventStatus::Success,
        json!({
            "lease_id": lease.lease_id,
            "release_reason": TaskReservationReleaseReason::Explicit.as_str(),
            "released_at": lease.released_at,
            "released_by": released_by,
        }),
    )?;
    queue::grant_queued(runtime, RELEASE_TOOL)?;
    Ok(json!({"released": true, "lease": lease_json(&lease)}))
}

/// One lease in any state. Leases past their TTL are ended first, so an
/// expired lease reads as `expired` rather than `granted`.
pub fn show(runtime: &RuntimeKernel, input: Value) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let lease_id = required_lease_id(&input)?;
    let orbit_dir = workspace_orbit_dir(runtime);
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let leases = runtime.stores().task_reservations();
    let listed = leases.list_active_leases(&orbit_dir, workspace_id.as_deref())?;
    emit_expired_lease_events(runtime, SHOW_TOOL, &listed.expired_leases)?;
    if !listed.expired_leases.is_empty() {
        queue::grant_queued(runtime, SHOW_TOOL)?;
    }
    let lease = leases
        .get_lease(&orbit_dir, workspace_id.as_deref(), &lease_id)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!("no lease `{lease_id}` in this workspace"))
        })?;
    Ok(json!({"lease": lease_json(&lease)}))
}

/// The workspace's active leases, advisory, hard and queued, oldest first.
pub fn list(runtime: &RuntimeKernel) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let store = runtime.stores().task_reservations();
    let orbit_dir = workspace_orbit_dir(runtime);
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let mut listed = store.list_active_leases(&orbit_dir, workspace_id.as_deref())?;
    emit_expired_lease_events(runtime, LIST_TOOL, &listed.expired_leases)?;
    if !listed.expired_leases.is_empty() && !queue::grant_queued(runtime, LIST_TOOL)?.is_empty() {
        listed.leases = store.inspect_active_leases(&orbit_dir, workspace_id.as_deref())?;
    }
    Ok(json!({
        "leases": listed.leases.iter().map(lease_json).collect::<Vec<_>>(),
    }))
}

/// Read one bounded page of durable notices strictly after `after`. The cursor
/// is the last returned event, so concurrent appends cannot be skipped.
pub fn notices(runtime: &RuntimeKernel, input: Value) -> Result<Value, OrbitError> {
    runtime.ensure_coordination_task_write_permitted()?;
    runtime.lease_policy().ensure_enabled()?;
    let integer = |field: &str, default: u64| -> Result<u64, OrbitError> {
        match input.get(field) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .filter(|n| *n <= i64::MAX as u64)
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "`{field}` must be a non-negative integer no larger than {}",
                        i64::MAX
                    ))
                }),
        }
    };
    let after = integer("after", 0)?;
    let limit = integer("limit", 100)?;
    if !(1..=500).contains(&limit) {
        return Err(OrbitError::InvalidInput(
            "`limit` must be between 1 and 500".into(),
        ));
    }
    let lease_id = optional_string_alias(&input, &["lease_id"])?;
    if let Some(id) = &lease_id {
        validate_lease_id_form(id)?;
    }
    let notices = runtime.stores().task_reservations().list_lease_notices(
        &workspace_orbit_dir(runtime),
        workspace_task_reservation_id(runtime)?.as_deref(),
        lease_id.as_deref(),
        after,
        limit as u32,
    )?;
    let cursor = notices.last().map_or(after, |notice| notice.seq);
    Ok(json!({"notices": notices, "cursor": cursor}))
}

/// For each of `paths`: whether the caller's `lease_id` covers it, whether
/// it matches `workflow.unlocked_context_paths`, and which other holders
/// (task context locks, task reservations and leases) overlap it. A read: it
/// expires, releases and audits nothing.
pub fn check(runtime: &RuntimeKernel, input: Value) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let repo_root = runtime.paths().repo_root.as_path();
    let paths = parse_selectors(&input, &["paths", "selectors", "files"], "paths", repo_root)?;
    let lease_id = optional_string_alias(&input, &["lease_id", "leaseId", "lease-id"])?;
    if let Some(lease_id) = lease_id.as_deref() {
        validate_lease_id_form(lease_id)?;
    }
    let caller_lease = match lease_id.as_deref() {
        Some(lease_id) => runtime
            .stores()
            .task_reservations()
            .get_lease(
                &workspace_orbit_dir(runtime),
                workspace_task_reservation_id(runtime)?.as_deref(),
                lease_id,
            )?
            .filter(lease_active),
        None => None,
    };
    let index = TaskLockIndex::load(runtime, &[])?;
    let holders = PathHolders::load(runtime, &index, lease_id.as_deref())?;
    let unlocked = runtime.unlocked_context_paths();
    let checked = paths
        .iter()
        .map(|path| {
            let covered = caller_lease.as_ref().is_some_and(|lease| {
                lease
                    .selectors
                    .iter()
                    .any(|selector| selector_covers(selector, path))
            });
            json!({
                "path": path,
                "covered": covered,
                "unlocked": unlocked.unlocks(path),
                "holders": holders.overlapping(
                    std::slice::from_ref(path),
                    repo_root,
                    &mut ResponseBudget::unbounded(),
                ),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "lease_id": lease_id,
        "lease_active": caller_lease.is_some(),
        "paths": checked,
    }))
}
