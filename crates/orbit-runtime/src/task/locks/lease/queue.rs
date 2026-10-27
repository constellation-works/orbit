//! The hard-lease queue's runtime half: `expand`, and the pass that grants
//! waiting requests and expansions once their holders release, expire or
//! settle.
//!
//! The store owns queue order, aging and deadlock refusal, transactionally;
//! the runtime supplies what only it can read, the task context locks on
//! each waiting selector. A lease never preempts a task: while an
//! `in-progress` or `review` task holds the paths, the lease waits for it to
//! settle or for its own TTL. The pass runs after a lease release, after any
//! operation that lazily expires a lease, and on every owner clock tick, which
//! is how a task's settlement or a task reservation's release reaches the
//! queue.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_store::contracts::{
    LeaseExpandParams, LeaseMode, LeaseQueueGrant, LeaseQueueGrantParams, LeaseQueueKind,
};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use super::super::audit::{emit_expired_lease_events, record_lease_audit_event};
use super::super::commands::{
    reservation_actor_label, workspace_orbit_dir, workspace_task_reservation_id,
};
use super::super::index::{
    TaskLockIndex, task_lock_conflicts_bounded, task_lock_conflicts_indexed,
};
use super::{
    LEASE_INTAKE_ACTOR, LeaseTokenGrant, PathHolders, ResponseBudget, TOKEN_MAX_LEASE_SELECTORS,
    ensure_token_covers_lease, ensure_token_selector_count, lease_active, lease_json,
    parse_selectors, required_lease_id, selector_covers,
};
use crate::RuntimeKernel;

const EXPAND_TOOL: &str = "orbit.lease.expand";

/// Add selectors to a granted lease.
///
/// An advisory lease takes them all at once and names the holders they
/// overlap. A hard lease drops `workflow.unlocked_context_paths` and the
/// selectors it already covers, takes the free rest at once and queues the
/// blocked ones while it keeps what it holds. When the queued expansion
/// closes a cycle of leases each waiting on another's held paths, the owner
/// refuses the expansion ranked last (lowest priority, then youngest) with a
/// `deadlock` notice; `deadlock` in the answer is set when that is this one.
pub fn expand(
    runtime: &RuntimeKernel,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let actor = reservation_actor_label(runtime, agent.as_deref(), model.as_deref())?;
    expand_as(runtime, input, actor, None)
}

/// Expand a lease for an authenticated lease token, refused without change
/// when the lease's mode or priority exceeds the token or its class policy,
/// when the lease would hold and wait for more than the token selector limit
/// together, or when its answer could not fit the token transport.
pub fn expand_with_token(
    runtime: &RuntimeKernel,
    input: Value,
    grant: &LeaseTokenGrant,
) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    ensure_token_covers_lease(runtime, &input, grant)?;
    expand_as(runtime, input, LEASE_INTAKE_ACTOR.to_string(), Some(grant))
}

fn expand_as(
    runtime: &RuntimeKernel,
    input: Value,
    actor: String,
    token: Option<&LeaseTokenGrant>,
) -> Result<Value, OrbitError> {
    let lease_id = required_lease_id(&input)?;
    let repo_root = runtime.paths().repo_root.as_path();
    let selectors = parse_selectors(&input, &["selectors", "files"], "selectors", repo_root)?;
    let orbit_dir = workspace_orbit_dir(runtime);
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let store = runtime.stores().task_reservations();
    let not_active = || {
        OrbitError::InvalidInput(format!(
            "lease `{lease_id}` is not active (unknown, released or expired); request a new lease"
        ))
    };
    let current = store
        .get_lease(&orbit_dir, workspace_id.as_deref(), &lease_id)?
        .filter(lease_active)
        .ok_or_else(not_active)?;
    if current.queue_kind == Some(LeaseQueueKind::Request) {
        return Err(OrbitError::InvalidInput(format!(
            "lease `{lease_id}` is still queued; expand it once it is granted, or release it and \
             request the wider selectors"
        )));
    }
    let (surface, unlocked_selectors) = match current.mode {
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
        LeaseMode::Advisory => (selectors, Vec::new()),
    };
    let surface = surface
        .into_iter()
        .filter(|selector| {
            !current
                .selectors
                .iter()
                .any(|held| selector_covers(held, selector))
        })
        // The store also removes exact queued duplicates before enforcing its
        // token limit. Mirror that here so retries do not inflate the early
        // held + queued + incoming count.
        .filter(|selector| token.is_none() || !current.queued_selectors.contains(selector))
        .collect::<Vec<_>>();
    let mut budget = match token {
        None => ResponseBudget::unbounded(),
        Some(grant) => {
            ensure_token_selector_count(
                current.selectors.len() + current.queued_selectors.len() + surface.len(),
            )?;
            // The largest answer with empty holder lists and no cycle yet.
            // `deadlock` is null here; the store spends the leftover budget
            // on the complete cycle or refuses the expansion before writing.
            let mut provisional = current.clone();
            provisional.selectors.extend(surface.iter().cloned());
            provisional.queued_selectors.extend(surface.iter().cloned());
            ResponseBudget::for_token(
                grant.max_response_bytes,
                &json!({
                    "added": surface,
                    "queued": surface,
                    "queue_position": u32::MAX,
                    "conflicts": [],
                    "deadlock": null,
                    "unlocked_selectors": unlocked_selectors,
                    "overlaps": [],
                    "truncated": false,
                    "lease": lease_json(&provisional),
                }),
            )?
        }
    };
    // JSON `null` is four bytes. A cycle may grow into the holder slack; the
    // store then shortens `conflicts` so the two share that slack.
    let deadlock_budget_bytes = match token {
        Some(_) => budget.holder_list_bytes().map(|slack| slack + 4),
        None => None,
    };
    let index = TaskLockIndex::load(runtime, &[])?;
    let task_conflicts = if current.mode == LeaseMode::Hard && !surface.is_empty() {
        runtime.reconcile_stale_owned_reservations_for_files(&surface, 32)?;
        task_lock_conflicts_bounded(&index, &surface, repo_root, budget.conflicts_shown())
    } else {
        Vec::new()
    };
    let result = store.expand_lease(LeaseExpandParams {
        workspace_orbit_dir: orbit_dir,
        workspace_id,
        lease_id: lease_id.clone(),
        selectors: surface,
        task_conflicts,
        aging_minutes: runtime.lease_policy().aging_minutes,
        holder_list_bytes: budget.holder_list_bytes(),
        max_selectors: token.map(|_| TOKEN_MAX_LEASE_SELECTORS as u32),
        deadlock_budget_bytes,
    })?;
    emit_expired_lease_events(runtime, EXPAND_TOOL, &result.expired_leases)?;
    if !result.expired_leases.is_empty() {
        grant_queued(runtime, EXPAND_TOOL)?;
    }
    if result.selector_limit_exceeded {
        // Checked above too; the store's check holds against a concurrent
        // expansion of the same lease.
        return Err(OrbitError::InvalidInput(format!(
            "lease `{lease_id}` would have more than the lease token limit of \
             {TOKEN_MAX_LEASE_SELECTORS} selectors; it is unchanged"
        )));
    }
    let lease = result.lease.ok_or_else(not_active)?;
    let conflicts = budget.take(result.conflicts);
    budget.truncated |= result.holders_truncated;
    let deadlock = result
        .deadlocks
        .iter()
        .find(|deadlock| deadlock.lease_id == lease_id);
    let overlaps = if lease.mode == LeaseMode::Advisory {
        PathHolders::load(runtime, &index, Some(&lease_id))?.overlapping(
            &result.added,
            repo_root,
            &mut budget,
        )
    } else {
        Vec::new()
    };
    record_lease_audit_event(
        runtime,
        "lease.expanded",
        EXPAND_TOOL,
        Some(lease_id.as_str()),
        AuditEventStatus::Success,
        json!({
            "lease_id": lease_id,
            "actor": actor,
            "added": result.added,
            "queued": result.queued,
            "unlocked_selectors": unlocked_selectors,
            "conflicts": conflicts,
            "queue_position": result.queue_position,
            "deadlocks": result.deadlocks,
        }),
    )?;
    for refused in &result.deadlocks {
        record_lease_audit_event(
            runtime,
            "lease.expand.deadlock",
            EXPAND_TOOL,
            Some(refused.lease_id.as_str()),
            AuditEventStatus::Denied,
            json!(refused),
        )?;
    }
    Ok(json!({
        "added": result.added,
        "queued": result.queued,
        "queue_position": result.queue_position,
        "conflicts": conflicts,
        "deadlock": deadlock,
        "unlocked_selectors": unlocked_selectors,
        "overlaps": overlaps,
        "truncated": budget.truncated,
        "lease": lease_json(&lease),
    }))
}

/// Grant every waiting request and expansion whose paths are now free, in
/// queue order, and audit each grant. Returns what was granted.
pub(super) fn grant_queued(
    runtime: &RuntimeKernel,
    tool: &str,
) -> Result<Vec<LeaseQueueGrant>, OrbitError> {
    let orbit_dir = workspace_orbit_dir(runtime);
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let store = runtime.stores().task_reservations();
    let waiting = store
        .inspect_active_leases(&orbit_dir, workspace_id.as_deref())?
        .into_iter()
        .filter(|lease| lease_active(lease) && !lease.queued_selectors.is_empty())
        .collect::<Vec<_>>();
    if waiting.is_empty() {
        return Ok(Vec::new());
    }
    let index = TaskLockIndex::load(runtime, &[])?;
    let repo_root = runtime.paths().repo_root.as_path();
    let task_conflicts = waiting
        .iter()
        .map(|lease| {
            (
                lease.lease_id.clone(),
                task_lock_conflicts_indexed(&index, &[], &lease.queued_selectors, repo_root),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let result = store.grant_queued_leases(LeaseQueueGrantParams {
        workspace_orbit_dir: orbit_dir,
        workspace_id,
        task_conflicts,
        aging_minutes: runtime.lease_policy().aging_minutes,
    })?;
    emit_expired_lease_events(runtime, tool, &result.expired_leases)?;
    for grant in &result.granted {
        let command = match grant.kind {
            LeaseQueueKind::Request => "lease.request.granted",
            LeaseQueueKind::Expansion => "lease.expand.granted",
        };
        record_lease_audit_event(
            runtime,
            command,
            tool,
            Some(grant.lease_id.as_str()),
            AuditEventStatus::Success,
            json!({
                "lease_id": grant.lease_id,
                "from_queue": true,
                "selectors": grant.selectors,
                "still_queued": grant.still_queued,
            }),
        )?;
    }
    for refused in &result.deadlocks {
        record_lease_audit_event(
            runtime,
            "lease.expand.deadlock",
            tool,
            Some(refused.lease_id.as_str()),
            AuditEventStatus::Denied,
            json!(refused),
        )?;
    }
    Ok(result.granted)
}
