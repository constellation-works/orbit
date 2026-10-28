//! Lease-token admission: what an authenticated token allows, the intake
//! actor and its audit rows, and the per-token bounds checked before any
//! write.

use orbit_common::OrbitError;
use orbit_store::contracts::{LeasePrincipalClass, LeasePriority};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::Value;

use super::super::audit::record_lease_audit_event;
use super::operations::show;
use super::policy::{lower_priority, parse_priority, priority_rank};
use crate::RuntimeKernel;

/// Active (granted or queued) leases one lease-token principal may have in a
/// workspace. Counted from the owner's lease rows by principal, so a new
/// token for the same principal, or a listener restart, starts nowhere new.
pub(super) const TOKEN_MAX_ACTIVE_LEASES: u32 = 64;

/// Selectors one lease-token lease may hold and wait for, together. It bounds
/// the overlap notices one request or expansion writes per holder and the
/// selector pairs the owner compares.
pub(super) const TOKEN_MAX_LEASE_SELECTORS: usize = 64;

/// What an authenticated lease token allows, as the lease intake socket
/// delivers it: the principal and class the token names, its id, its
/// priority ceiling and `hard` permission, and the client address the
/// listener saw. The listener user is trusted for this identity.
#[derive(Clone, Debug)]
pub struct LeaseTokenGrant {
    /// The token's principal; it replaces any principal in the input.
    pub principal: String,
    /// The token's principal class, which selects the workspace's
    /// `lease.hard.<class>` and `lease.max_priority.<class>`.
    pub principal_class: LeasePrincipalClass,
    /// The token's public id, recorded on the lease and in audit rows.
    pub token_id: String,
    /// The token's priority ceiling.
    pub max_priority: LeasePriority,
    /// Whether the token may hold hard leases.
    pub hard_allowed: bool,
    /// The client address the listener resolved.
    pub client_address: String,
    /// The largest serialized answer the transport can deliver. A request or
    /// expansion whose answer could not fit is refused before anything is
    /// written, and holder lists are cut to fit (`truncated: true`).
    pub max_response_bytes: usize,
}

/// The actor recorded on leases granted through the lease intake socket, and
/// the tool name of its audit rows.
pub(super) const LEASE_INTAKE_ACTOR: &str = "lease-intake";

/// Record one lease intake operation as `lease.intake.<operation>`.
///
/// The intake socket writes one row per operation it serves, granted,
/// refused or failed, with the principal, token id, client address and
/// outcome in `payload`; the operation's own rows are written as usual.
pub fn record_lease_intake_audit(
    runtime: &RuntimeKernel,
    operation: &str,
    lease_id: Option<&str>,
    status: AuditEventStatus,
    payload: Value,
) -> Result<(), OrbitError> {
    record_lease_audit_event(
        runtime,
        &format!("lease.intake.{operation}"),
        LEASE_INTAKE_ACTOR,
        lease_id,
        status,
        payload,
    )
}

/// Refuse a token request or expansion that would leave a lease with more
/// than [`TOKEN_MAX_LEASE_SELECTORS`] selectors, before anything is written.
pub(super) fn ensure_token_selector_count(count: usize) -> Result<(), OrbitError> {
    if count <= TOKEN_MAX_LEASE_SELECTORS {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "a lease token's lease may have at most {TOKEN_MAX_LEASE_SELECTORS} selectors; this \
         one would have {count}. Lease a covering `dir:` selector instead"
    )))
}

/// Refuse a renewal or expansion of a lease whose mode or priority exceeds
/// the calling token or its class policy, leaving the lease unchanged.
pub(super) fn ensure_token_covers_lease(
    runtime: &RuntimeKernel,
    input: &Value,
    grant: &LeaseTokenGrant,
) -> Result<(), OrbitError> {
    let found = show(runtime, input.clone())?;
    let lease = &found["lease"];
    let policy = runtime.lease_policy();
    let ceiling = lower_priority(
        policy.max_priority(grant.principal_class),
        grant.max_priority,
    );
    let priority = lease
        .get("priority")
        .and_then(Value::as_str)
        .ok_or_else(|| OrbitError::InvalidInput("lease is not active".to_string()))?;
    let priority = parse_priority(priority, "priority")?;
    let hard = lease.get("mode").and_then(Value::as_str) == Some("hard");
    if priority_rank(priority) > priority_rank(ceiling)
        || (hard && !(grant.hard_allowed && policy.hard_allowed(grant.principal_class)))
    {
        return Err(OrbitError::InvalidInput(
            "lease exceeds the calling token's or workspace policy's permissions".to_string(),
        ));
    }
    Ok(())
}
