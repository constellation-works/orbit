//! The exclusive workspace claim, and the gate it puts on workflow dispatch
//! [ADR-0352, ORB-10709].
//!
//! Workspace ownership binds a workspace to a *machine*. It does not say which
//! operator is driving it right now, and an off-box orchestrator over the owned
//! tunnel, a local operator broker, and a session over SSH can all reach one
//! workspace concurrently. Nothing arbitrated dispatch between them: the
//! duplicate-dispatch guard is keyed on task id over a bounded window, and
//! discovery-mode submissions carry no task ids at all.
//!
//! What this gates, and what it deliberately does not:
//!
//! - **Only the governed workflow operations.** Filing tasks, reads, updates,
//!   search, knowledge, and friction stay concurrent. Several people working
//!   different features in one workspace is the intended behaviour; only the
//!   decision of *what starts* is serialized.
//! - **At the shared run-submission path**, not at a protocol adapter, so CLI,
//!   HTTP, MCP, and remote execution inherit the same refusal — and a caller
//!   holding a shell cannot route around it, because the CLI reaches the same
//!   chokepoint.
//! - **Keyed on a minted token**, never on MCP session identity, which is minted
//!   per connection and cleared when client-supplied: keying on it would orphan
//!   the workspace on every reconnect. Machine and session are recorded for
//!   diagnostics only.
//!
//! An unclaimed workspace gates nothing. The claim is an arbitration between
//! operators who want one, not a mandatory ceremony before every dispatch.

use orbit_common::protocol::tool_input::{optional_string, optional_u32_alias};
use orbit_common::{OrbitError, WorkspaceClaimHeld};
use orbit_store::contracts::{
    WorkspaceClaimAcquireParams, WorkspaceClaimCheckParams, WorkspaceClaimHolder,
    WorkspaceClaimReleaseParams,
};

use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use super::coordination_audit::{CoordinationAuditEvent, record_coordination_audit_event};
use super::task::locks::{workspace_orbit_dir, workspace_task_reservation_id};
use crate::OrbitRuntime;

/// Environment fallback for the holder's token, so an operator shell does not
/// have to repeat `--claim-token` on every dispatch. An explicit argument always
/// wins; this is a convenience, not a second identity.
pub const CLAIM_TOKEN_ENV: &str = "ORBIT_WORKSPACE_CLAIM_TOKEN";

const DEFAULT_CLAIM_TTL_SECONDS: u32 = 3600;
const MAX_CLAIM_TTL_SECONDS: u32 = 43_200;
const CLAIM_TARGET_TYPE: &str = "workspace_claim";

pub(crate) fn acquire(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let ttl_seconds = optional_u32_alias(&input, &["ttl_seconds", "ttlSeconds", "ttl-seconds"])?
        .unwrap_or(DEFAULT_CLAIM_TTL_SECONDS);
    if !(1..=MAX_CLAIM_TTL_SECONDS).contains(&ttl_seconds) {
        return Err(OrbitError::InvalidInput(format!(
            "`ttl_seconds` must be between 1 and {MAX_CLAIM_TTL_SECONDS} seconds"
        )));
    }
    let actor = claim_actor_label(runtime, agent.as_deref(), model.as_deref())?;
    let result = runtime
        .stores()
        .task_reservations()
        .acquire_workspace_claim(WorkspaceClaimAcquireParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime)?,
            actor: actor.clone(),
            ttl_seconds,
            machine_id: optional_string(&input, "machine_id")?,
            session_id: optional_string(&input, "session_id")?,
        })?;
    record_expired_claims(runtime, &result.expired_claims)?;

    if let Some(claim) = result.claim {
        let claim_token = result.claim_token.ok_or_else(|| {
            OrbitError::Execution("workspace claim grant is missing its token".to_string())
        })?;
        record_claim_audit(
            runtime,
            "workspace.claim.acquired",
            "orbit.workspace.claim.acquire",
            Some(claim.claim_id.as_str()),
            AuditEventStatus::Success,
            // The token is never audited: an audit reader is not the holder.
            json!({
                "claim_id": claim.claim_id,
                "actor": claim.actor,
                "expires_at": claim.expires_at,
                "ttl_seconds": ttl_seconds,
                "machine_id": claim.machine_id,
                "session_id": claim.session_id,
            }),
        )?;
        return Ok(json!({
            "acquired": true,
            "claim_token": claim_token,
            "claim": holder_json(&claim),
        }));
    }

    let conflict = result.conflict.ok_or_else(|| {
        OrbitError::Execution("refused workspace claim is missing its incumbent".to_string())
    })?;
    record_claim_audit(
        runtime,
        "workspace.claim.denied",
        "orbit.workspace.claim.acquire",
        Some(conflict.claim_id.as_str()),
        AuditEventStatus::Denied,
        json!({
            "requested_by": actor,
            "held_by": conflict.actor,
            "claim_id": conflict.claim_id,
            "expires_at": conflict.expires_at,
        }),
    )?;
    Ok(json!({
        "acquired": false,
        "claim": holder_json(&conflict),
    }))
}

pub(crate) fn release(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let force = input.get("force").and_then(Value::as_bool).unwrap_or(false);
    let claim_token = optional_string(&input, "claim_token")?;
    if claim_token.is_none() && !force {
        return Err(OrbitError::InvalidInput(
            "releasing a workspace claim requires the holder's `claim_token`, or `force: true` to displace the holder (which is audited)"
                .to_string(),
        ));
    }
    let released_by = claim_actor_label(runtime, agent.as_deref(), model.as_deref())?;
    let result = runtime
        .stores()
        .task_reservations()
        .release_workspace_claim(WorkspaceClaimReleaseParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime)?,
            claim_token,
            force,
            released_by: released_by.clone(),
        })?;
    record_expired_claims(runtime, &result.expired_claims)?;

    let Some(claim) = result.claim else {
        return Ok(json!({ "released": false, "reason": "no active workspace claim" }));
    };

    if !result.released {
        record_claim_audit(
            runtime,
            "workspace.claim.release.denied",
            "orbit.workspace.claim.release",
            Some(claim.claim_id.as_str()),
            AuditEventStatus::Denied,
            json!({
                "requested_by": released_by,
                "held_by": claim.actor,
                "claim_id": claim.claim_id,
                "expires_at": claim.expires_at,
            }),
        )?;
        return Ok(json!({
            "released": false,
            "reason": "presented token does not match the active claim",
            "claim": holder_json(&claim),
        }));
    }

    // A force-release is the escape hatch for a holder that has gone away, and
    // it is what makes the guarantee advisory if it becomes habitual. The record
    // therefore names both parties, so a reader can tell a holder tidying up
    // after itself apart from one operator displacing another.
    record_claim_audit(
        runtime,
        if result.forced {
            "workspace.claim.force_released"
        } else {
            "workspace.claim.released"
        },
        "orbit.workspace.claim.release",
        Some(claim.claim_id.as_str()),
        AuditEventStatus::Success,
        json!({
            "claim_id": claim.claim_id,
            "forced": result.forced,
            "released_by": released_by,
            "displaced_holder": claim.actor,
            "displaced_machine_id": claim.machine_id,
            "displaced_session_id": claim.session_id,
            "released_at": result.released_at,
        }),
    )?;
    Ok(json!({
        "released": true,
        "forced": result.forced,
        "released_at": result.released_at,
        "claim": holder_json(&claim),
    }))
}

pub(crate) fn show(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    let result = runtime.stores().task_reservations().show_workspace_claim(
        &workspace_orbit_dir(runtime),
        workspace_task_reservation_id(runtime)?.as_deref(),
    )?;
    record_expired_claims(runtime, &result.expired_claims)?;
    Ok(match result.claim {
        Some(claim) => json!({ "claimed": true, "claim": holder_json(&claim) }),
        None => json!({ "claimed": false }),
    })
}

impl OrbitRuntime {
    /// Refuse `operation` unless the caller may dispatch in this workspace.
    ///
    /// Three outcomes, and only the middle one refuses:
    ///
    /// 1. **Unclaimed.** Nothing to arbitrate; dispatch proceeds. The claim is
    ///    an opt-in hold, so an existing workspace keeps working unchanged until
    ///    someone takes one.
    /// 2. **Claimed by someone else.** Refuse with the holder and the expiry
    ///    instant, so the caller can wait, ask, or force rather than retry.
    /// 3. **Claimed, and the caller presented the holder's token.** Proceed.
    ///
    /// Nothing here is keyed on task ids, which is exactly why it covers the
    /// discovery-mode submissions the duplicate-dispatch guard cannot see.
    pub(crate) fn require_workspace_claim(
        &self,
        operation: &str,
        claim_token: Option<&str>,
    ) -> Result<(), OrbitError> {
        let presented = claim_token
            .map(str::to_string)
            .or_else(claim_token_from_env)
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty());
        let result =
            self.stores()
                .task_reservations()
                .check_workspace_claim(WorkspaceClaimCheckParams {
                    workspace_orbit_dir: workspace_orbit_dir(self),
                    workspace_id: workspace_task_reservation_id(self)?,
                    claim_token: presented,
                })?;
        record_expired_claims(self, &result.expired_claims)?;

        let Some(claim) = result.claim else {
            return Ok(());
        };
        if result.token_matches {
            return Ok(());
        }

        record_claim_audit(
            self,
            "workspace.claim.dispatch.denied",
            operation,
            Some(claim.claim_id.as_str()),
            AuditEventStatus::Denied,
            json!({
                "operation": operation,
                "held_by": claim.actor,
                "claim_id": claim.claim_id,
                "expires_at": claim.expires_at,
                "machine_id": claim.machine_id,
                "session_id": claim.session_id,
            }),
        )?;
        Err(OrbitError::WorkspaceClaimHeld(Box::new(
            WorkspaceClaimHeld {
                operation: operation.to_string(),
                holder: claim.actor,
                claim_id: claim.claim_id,
                expires_at: claim.expires_at,
            },
        )))
    }
}

fn claim_token_from_env() -> Option<String> {
    std::env::var(CLAIM_TOKEN_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn holder_json(claim: &WorkspaceClaimHolder) -> Value {
    json!({
        "claim_id": claim.claim_id,
        "workspace_id": claim.workspace_id,
        "actor": claim.actor,
        "created_at": claim.created_at,
        "expires_at": claim.expires_at,
        "machine_id": claim.machine_id,
        "session_id": claim.session_id,
    })
}

fn claim_actor_label(
    runtime: &OrbitRuntime,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<String, OrbitError> {
    runtime.actor().resolve_write_label(agent, model)
}

fn record_expired_claims(
    runtime: &OrbitRuntime,
    expired: &[orbit_store::contracts::ExpiredTaskReservation],
) -> Result<(), OrbitError> {
    for claim in expired {
        record_claim_audit(
            runtime,
            "workspace.claim.expired",
            "orbit.workspace.claim",
            Some(claim.reservation_id.as_str()),
            AuditEventStatus::Success,
            json!({
                "claim_id": claim.reservation_id,
                "expired_at": claim.expired_at,
            }),
        )?;
    }
    Ok(())
}

fn record_claim_audit(
    runtime: &OrbitRuntime,
    command: &str,
    tool_name: &str,
    claim_id: Option<&str>,
    status: AuditEventStatus,
    payload: Value,
) -> Result<(), OrbitError> {
    record_coordination_audit_event(
        runtime,
        CoordinationAuditEvent {
            command,
            tool_name,
            target_type: CLAIM_TARGET_TYPE,
            target_id: claim_id,
            task_id: None,
            status,
            payload,
        },
    )
}
