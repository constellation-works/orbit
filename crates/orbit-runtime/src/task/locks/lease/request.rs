//! Lease requests: the caller's and the lease token's entry points, the one
//! request path both share, and its refusals.

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{optional_string_alias, optional_string_list_alias};
use orbit_store::contracts::{
    LeaseInsertParams, LeaseMode, LeasePrincipalClass, LeasePriority, LeaseQueueKind, LeaseRecord,
    TaskLockConflict,
};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use super::super::audit::{emit_expired_lease_events, record_lease_audit_event};
use super::super::commands::{
    parse_ttl_seconds, reservation_actor_label, workspace_orbit_dir, workspace_task_reservation_id,
};
use super::super::index::{TaskLockIndex, task_lock_conflicts_bounded};
use super::admission::{
    LEASE_INTAKE_ACTOR, LeaseTokenGrant, TOKEN_MAX_ACTIVE_LEASES, ensure_token_selector_count,
};
use super::holders::PathHolders;
use super::input::{LEASE_ID_PREFIX, parse_selectors, required_non_empty};
use super::policy::{
    lease_principal_class, lower_priority, parse_mode, parse_priority, priority_rank,
};
use super::queue;
use super::response::{PLACEHOLDER_TIMESTAMP, ResponseBudget, lease_json};
use crate::RuntimeKernel;

const REQUEST_TOOL: &str = "orbit.lease.request";

/// Request a lease on `selectors` for `principal`.
///
/// Advisory is granted and names the holders it overlaps. Hard drops
/// `workflow.unlocked_context_paths` from its surface and is granted only
/// when the rest overlaps no task context lock, task reservation or hard
/// lease, and no queued request or expansion ranked ahead of it wants those
/// paths; otherwise it queues (`granted: false`, `queued: true`) with those
/// holders and its queue position, or with `wait = false` is refused with
/// them. A class whose
/// `lease.hard` is `never` is granted advisory instead, and the answer says
/// so. A priority above the class's `lease.max_priority` is refused.
pub fn request(
    runtime: &RuntimeKernel,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let principal = required_non_empty(&input, "principal")?;
    let actor = reservation_actor_label(runtime, agent.as_deref(), model.as_deref())?;
    request_as(
        runtime,
        input,
        LeaseRequester {
            principal,
            principal_class: lease_principal_class(),
            actor,
            token: None,
        },
    )
}

/// Request a lease for an authenticated lease token.
///
/// The same operation as [`request`], with the token's principal and class
/// instead of the caller's declared principal and process class, and with the
/// stricter of the token's scope and the workspace's class policy: the lower
/// of the two priority ceilings, and hard only when both allow it. The lease
/// records the token id, and its audit rows name the token and client. A
/// token is also held to `TOKEN_MAX_ACTIVE_LEASES` per principal, to
/// `TOKEN_MAX_LEASE_SELECTORS` per lease, and to answers that fit
/// [`LeaseTokenGrant::max_response_bytes`], each checked before any write.
pub fn request_with_token(
    runtime: &RuntimeKernel,
    input: Value,
    grant: &LeaseTokenGrant,
) -> Result<Value, OrbitError> {
    request_as(
        runtime,
        input,
        LeaseRequester {
            principal: grant.principal.clone(),
            principal_class: grant.principal_class,
            actor: LEASE_INTAKE_ACTOR.to_string(),
            token: Some(grant),
        },
    )
}

struct LeaseRequester<'a> {
    principal: String,
    principal_class: LeasePrincipalClass,
    actor: String,
    token: Option<&'a LeaseTokenGrant>,
}

fn request_as(
    runtime: &RuntimeKernel,
    input: Value,
    requester: LeaseRequester<'_>,
) -> Result<Value, OrbitError> {
    let policy = runtime.lease_policy();
    policy.ensure_enabled()?;
    let LeaseRequester {
        principal,
        principal_class,
        actor,
        token,
    } = requester;
    let repo_root = runtime.paths().repo_root.as_path();
    let selectors = parse_selectors(&input, &["selectors", "files"], "selectors", repo_root)?;
    let ttl_seconds = parse_ttl_seconds(&input)?;
    let class_ceiling = policy.max_priority(principal_class);
    let ceiling = match token {
        Some(grant) => lower_priority(class_ceiling, grant.max_priority),
        None => class_ceiling,
    };
    let priority = match optional_string_alias(&input, &["priority"])? {
        Some(raw) => parse_priority(&raw, "priority")?,
        None => lower_priority(LeasePriority::Normal, ceiling),
    };
    let requested_mode = match optional_string_alias(&input, &["mode"])? {
        Some(raw) => parse_mode(&raw, "mode")?,
        None => policy.default_mode,
    };
    let mut audit_request = json!({
        "actor": actor,
        "principal": principal,
        "principal_class": principal_class,
        "selectors": selectors,
        "requested_mode": requested_mode,
        "priority": priority,
        "ttl_seconds": ttl_seconds,
    });
    if let Some(grant) = token {
        audit_request["token_id"] = json!(grant.token_id);
        audit_request["client_address"] = json!(grant.client_address);
    }

    if priority_rank(priority) > priority_rank(ceiling) {
        let mut payload = audit_request;
        payload["reason"] = json!("priority_ceiling");
        payload["max_priority"] = json!(ceiling);
        record_lease_audit_event(
            runtime,
            "lease.request.refused",
            REQUEST_TOOL,
            None,
            AuditEventStatus::Denied,
            payload,
        )?;
        let limit = if priority_rank(class_ceiling) > priority_rank(ceiling) {
            "the lease token's priority ceiling".to_string()
        } else {
            format!("`lease.max_priority.{}`", principal_class.as_str())
        };
        return Err(OrbitError::InvalidInput(format!(
            "lease priority `{}` is above the `{}` principal ceiling `{}` ({limit})",
            priority.as_str(),
            principal_class.as_str(),
            ceiling.as_str(),
        )));
    }

    let class_allows_hard = policy.hard_allowed(principal_class);
    let token_allows_hard = token.is_none_or(|grant| grant.hard_allowed);
    let downgraded = requested_mode == LeaseMode::Hard && !(class_allows_hard && token_allows_hard);
    let mode = if downgraded {
        LeaseMode::Advisory
    } else {
        requested_mode
    };
    let (stored_selectors, unlocked_selectors) = match mode {
        LeaseMode::Hard => {
            let surface = runtime
                .unlocked_context_paths()
                .lock_surface(selectors.clone());
            let unlocked = selectors
                .iter()
                .filter(|selector| !surface.contains(selector))
                .cloned()
                .collect::<Vec<_>>();
            (surface, unlocked)
        }
        LeaseMode::Advisory => (selectors.clone(), Vec::new()),
    };

    let wait = match input.get("wait") {
        None | Some(Value::Null) => true,
        Some(Value::Bool(wait)) => *wait,
        Some(_) => {
            return Err(OrbitError::InvalidInput(
                "`wait` must be a boolean".to_string(),
            ));
        }
    };
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let tags = optional_string_list_alias(&input, &["tags"])?.unwrap_or_default();
    let machine_id = optional_string_alias(&input, &["machine_id", "remote_caller_machine_id"])?;
    let base_branch = Some(
        optional_string_alias(&input, &["base_branch", "base"])?
            .unwrap_or_else(|| runtime.workspace_base_branch().to_string()),
    );
    let bound_branch = optional_string_alias(&input, &["branch", "bound_branch"])?;
    let mut budget = match token {
        None => ResponseBudget::unbounded(),
        Some(grant) => {
            ensure_token_selector_count(selectors.len())?;
            // The largest answer this request can get before its holder
            // lists: a grant with a notice, or a queued answer.
            let provisional = LeaseRecord {
                lease_id: format!("{LEASE_ID_PREFIX}{}", "0".repeat(64)),
                notice_cursor: 0,
                workspace_id: workspace_id.clone(),
                selectors: stored_selectors.clone(),
                mode,
                priority,
                tags: tags.clone(),
                principal: principal.clone(),
                principal_class,
                machine_id: machine_id.clone(),
                base_branch: base_branch.clone(),
                bound_branch: bound_branch.clone(),
                actor: actor.clone(),
                created_at: PLACEHOLDER_TIMESTAMP.to_string(),
                expires_at: PLACEHOLDER_TIMESTAMP.to_string(),
                heartbeat_at: PLACEHOLDER_TIMESTAMP.to_string(),
                released_at: None,
                release_reason: None,
                token_id: Some(grant.token_id.clone()),
                queued_selectors: stored_selectors.clone(),
                queued_at: Some(PLACEHOLDER_TIMESTAMP.to_string()),
                queue_kind: Some(LeaseQueueKind::Request),
            };
            ResponseBudget::for_token(
                grant.max_response_bytes,
                &json!({
                    "granted": true,
                    "queued": true,
                    "queue_position": u32::MAX,
                    "requested_mode": requested_mode,
                    "downgraded": downgraded,
                    "unlocked_selectors": unlocked_selectors,
                    "overlaps": [],
                    "conflicts": [],
                    "queued_ahead": [],
                    "truncated": false,
                    "lease": lease_json(&provisional),
                    "notice": "x".repeat(128),
                }),
            )?
        }
    };
    let index = TaskLockIndex::load(runtime, &[])?;
    let task_conflicts = if mode == LeaseMode::Hard {
        runtime.reconcile_stale_owned_reservations_for_files(&stored_selectors, 32)?;
        task_lock_conflicts_bounded(
            &index,
            &stored_selectors,
            repo_root,
            budget.conflicts_shown(),
        )
    } else {
        Vec::new()
    };

    let result = runtime
        .stores()
        .task_reservations()
        .insert_lease(LeaseInsertParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id,
            selectors: stored_selectors.clone(),
            mode,
            priority,
            tags,
            principal: principal.clone(),
            principal_class,
            machine_id,
            base_branch,
            bound_branch,
            actor: actor.clone(),
            ttl_seconds,
            token_id: token.map(|grant| grant.token_id.clone()),
            wait,
            task_conflicts,
            aging_minutes: policy.aging_minutes,
            holder_list_bytes: budget.holder_list_bytes(),
            active_lease_limit: token.map(|_| TOKEN_MAX_ACTIVE_LEASES),
        })?;
    emit_expired_lease_events(runtime, REQUEST_TOOL, &result.expired_leases)?;
    if !result.expired_leases.is_empty() {
        queue::grant_queued(runtime, REQUEST_TOOL)?;
    }
    if let Some(active) = result.active_lease_limit_reached {
        return refuse_over_active_limit(runtime, audit_request, active);
    }
    let conflicts = budget.take(result.conflicts);
    let queued_ahead = budget.take(result.queued_ahead);
    budget.truncated |= result.holders_truncated;
    let Some(lease) = result.lease else {
        return refuse(
            runtime,
            audit_request,
            &stored_selectors,
            conflicts,
            &queued_ahead,
            budget.truncated,
        );
    };

    if lease.queue_kind == Some(LeaseQueueKind::Request) {
        let mut payload = audit_request;
        payload["lease_id"] = json!(lease.lease_id);
        payload["mode"] = json!(lease.mode);
        payload["hard_surface"] = json!(stored_selectors);
        payload["conflicts"] = json!(conflicts);
        payload["queued_ahead"] = json!(queued_ahead);
        payload["queue_position"] = json!(result.queue_position);
        record_lease_audit_event(
            runtime,
            "lease.request.queued",
            REQUEST_TOOL,
            Some(lease.lease_id.as_str()),
            AuditEventStatus::Success,
            payload,
        )?;
        return Ok(json!({
            "granted": false,
            "queued": true,
            "queue_position": result.queue_position,
            "conflicts": conflicts,
            "queued_ahead": queued_ahead,
            "truncated": budget.truncated,
            "requested_mode": requested_mode,
            "downgraded": false,
            "unlocked_selectors": unlocked_selectors,
            "lease": lease_json(&lease),
        }));
    }

    let overlaps = if mode == LeaseMode::Advisory {
        PathHolders::load(runtime, &index, Some(&lease.lease_id))?.overlapping(
            &lease.selectors,
            repo_root,
            &mut budget,
        )
    } else {
        Vec::new()
    };
    let notice = downgraded.then(|| {
        if class_allows_hard {
            "granted as advisory: the lease token does not allow hard leases".to_string()
        } else {
            format!(
                "granted as advisory: `lease.hard.{}` is `never` for this principal class",
                principal_class.as_str()
            )
        }
    });
    let mut granted_payload = json!({
            "lease_id": lease.lease_id,
            "actor": actor,
            "principal": lease.principal,
            "principal_class": lease.principal_class,
            "selectors": lease.selectors,
            "unlocked_selectors": unlocked_selectors,
            "requested_mode": requested_mode,
            "mode": lease.mode,
            "downgraded": downgraded,
            "priority": lease.priority,
            "expires_at": lease.expires_at,
    });
    if let Some(grant) = token {
        granted_payload["token_id"] = json!(grant.token_id);
        granted_payload["client_address"] = json!(grant.client_address);
    }
    record_lease_audit_event(
        runtime,
        "lease.request.granted",
        REQUEST_TOOL,
        Some(lease.lease_id.as_str()),
        AuditEventStatus::Success,
        granted_payload,
    )?;
    let mut output = json!({
        "granted": true,
        "requested_mode": requested_mode,
        "downgraded": downgraded,
        "unlocked_selectors": unlocked_selectors,
        "overlaps": overlaps,
        "truncated": budget.truncated,
        "lease": lease_json(&lease),
    });
    if let Some(notice) = notice {
        output["notice"] = json!(notice);
    }
    Ok(output)
}

fn refuse(
    runtime: &RuntimeKernel,
    mut payload: Value,
    hard_surface: &[String],
    conflicts: Vec<TaskLockConflict>,
    queued_ahead: &[String],
    truncated: bool,
) -> Result<Value, OrbitError> {
    payload["reason"] = json!("held");
    payload["hard_surface"] = json!(hard_surface);
    payload["conflicts"] = json!(conflicts);
    payload["queued_ahead"] = json!(queued_ahead);
    record_lease_audit_event(
        runtime,
        "lease.request.refused",
        REQUEST_TOOL,
        None,
        AuditEventStatus::Denied,
        payload,
    )?;
    Ok(json!({
        "granted": false,
        "queued": false,
        "conflicts": conflicts,
        "queued_ahead": queued_ahead,
        "truncated": truncated,
    }))
}

/// Refuse a token request whose principal already has
/// [`TOKEN_MAX_ACTIVE_LEASES`] active leases; the store wrote nothing.
fn refuse_over_active_limit(
    runtime: &RuntimeKernel,
    mut payload: Value,
    active: u32,
) -> Result<Value, OrbitError> {
    payload["reason"] = json!("active_lease_limit");
    payload["active_leases"] = json!(active);
    payload["limit"] = json!(TOKEN_MAX_ACTIVE_LEASES);
    record_lease_audit_event(
        runtime,
        "lease.request.refused",
        REQUEST_TOOL,
        None,
        AuditEventStatus::Denied,
        payload,
    )?;
    Ok(json!({
        "granted": false,
        "queued": false,
        "reason": "active_lease_limit",
        "active_leases": active,
        "limit": TOKEN_MAX_ACTIVE_LEASES,
        "message": format!(
            "this principal already has {active} active leases in this workspace, the lease \
             token limit of {TOKEN_MAX_ACTIVE_LEASES}; release one before requesting another"
        ),
        "conflicts": [],
        "queued_ahead": [],
        "truncated": false,
    }))
}
