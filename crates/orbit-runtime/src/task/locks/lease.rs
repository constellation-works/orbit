//! Path-lease operations (`docs/design/path-leases`): request, renew,
//! expand, release, show, list, check and notices over the lease rows the
//! reservation store keeps.
//!
//! A lease is held by a principal (a person or an agent), never by a task.
//! Advisory leases are always granted and block nothing; a hard lease is
//! granted only when no selector of its hard surface overlaps a task context
//! lock, a task reservation or another hard lease. An overlapping hard
//! request waits in the lease queue (`queue`), holding nothing, unless it
//! asked `wait = false`, which refuses it with the holders named.
//!
//! Over SSH the principal class and its `lease.*` ceilings are attribution
//! plus a guard, not a security boundary. A lease token is bounded further:
//! its principal's active leases, each lease's selectors, and the size of
//! every answer it gets (see `TOKEN_MAX_ACTIVE_LEASES`).

use std::ops::ControlFlow;
use std::path::Path;

mod lifetime;
mod queue;
pub use lifetime::sweep_lease_lifetimes;
pub use queue::{expand, expand_with_token};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::selector::{OverlapScope, overlaps};
use orbit_common::protocol::tool_input::{
    optional_string_alias, optional_string_list_alias, required_string,
};
use orbit_store::contracts::{
    LeaseInsertParams, LeaseMode, LeaseQueueKind, LeaseRecord, LeaseReleaseParams,
    LeaseRenewParams, TaskLockConflict, TaskLockHolder, TaskReservationKind,
    TaskReservationReleaseReason,
};
pub use orbit_store::contracts::{LeasePrincipalClass, LeasePriority};
use orbit_types::telemetry::AuditEventStatus;
use serde::Serialize;
use serde_json::{Value, json};

use super::audit::{emit_expired_lease_events, record_lease_audit_event};
use super::commands::{
    canonicalize_file_lock_selectors, parse_file_lock_selectors, parse_ttl_seconds,
    reservation_actor_label, workspace_orbit_dir, workspace_task_reservation_id,
};
use super::index::{
    TaskLockIndex, holds_context_lock, task_lock_conflicts_bounded, task_lock_status_rank,
};
use crate::RuntimeKernel;

const REQUEST_TOOL: &str = "orbit.lease.request";
const RENEW_TOOL: &str = "orbit.lease.renew";
const RELEASE_TOOL: &str = "orbit.lease.release";
const SHOW_TOOL: &str = "orbit.lease.show";
const LIST_TOOL: &str = "orbit.lease.list";
const LEASE_ID_PREFIX: &str = "lease-";

/// Active (granted or queued) leases one lease-token principal may have in a
/// workspace. Counted from the owner's lease rows by principal, so a new
/// token for the same principal, or a listener restart, starts nowhere new.
const TOKEN_MAX_ACTIVE_LEASES: u32 = 64;

/// Selectors one lease-token lease may hold and wait for, together. It bounds
/// the overlap notices one request or expansion writes per holder and the
/// selector pairs the owner compares.
const TOKEN_MAX_LEASE_SELECTORS: usize = 64;

/// Room kept free in a token answer's size estimate for what the estimate
/// cannot know before the write: the response envelope, the final lease id
/// and timestamps, and a queue position.
const TOKEN_RESPONSE_MARGIN_BYTES: usize = 512;

/// `lease.*` as admitted: whether leases are accepted, the mode a request
/// that names none gets, and the per-principal-class guards.
#[derive(Clone, Debug)]
pub(crate) struct LeasePolicy {
    enabled: bool,
    default_mode: LeaseMode,
    hard_allowed_human: bool,
    hard_allowed_agent: bool,
    max_priority_human: LeasePriority,
    max_priority_agent: LeasePriority,
    aging_minutes: u32,
}

impl LeasePolicy {
    /// Read the policy from admitted settings. Config admission already
    /// refused any value outside each key's choices.
    pub(crate) fn admit(snapshot: &orbit_config::ConfigSnapshot) -> Result<Self, OrbitError> {
        Ok(Self {
            enabled: snapshot.lease_enabled,
            default_mode: parse_mode(&snapshot.lease_default_mode, "lease.default_mode")?,
            hard_allowed_human: hard_allowed(&snapshot.lease_hard_human, "lease.hard.human")?,
            hard_allowed_agent: hard_allowed(&snapshot.lease_hard_agent, "lease.hard.agent")?,
            max_priority_human: parse_priority(
                &snapshot.lease_max_priority_human,
                "lease.max_priority.human",
            )?,
            max_priority_agent: parse_priority(
                &snapshot.lease_max_priority_agent,
                "lease.max_priority.agent",
            )?,
            aging_minutes: snapshot.lease_aging_minutes,
        })
    }

    fn ensure_enabled(&self) -> Result<(), OrbitError> {
        if self.enabled {
            return Ok(());
        }
        Err(OrbitError::InvalidInput(
            "path leases are disabled in this workspace; set `lease.enabled = true` to accept \
             lease operations"
                .to_string(),
        ))
    }

    fn hard_allowed(&self, class: LeasePrincipalClass) -> bool {
        match class {
            LeasePrincipalClass::Human => self.hard_allowed_human,
            LeasePrincipalClass::Agent => self.hard_allowed_agent,
        }
    }

    fn max_priority(&self, class: LeasePrincipalClass) -> LeasePriority {
        match class {
            LeasePrincipalClass::Human => self.max_priority_human,
            LeasePrincipalClass::Agent => self.max_priority_agent,
        }
    }
}

/// The principal class of the calling context: `agent` when the process
/// declares an agent or a managed run (the signal
/// `agent_context_declared` reads), otherwise `human`.
pub fn lease_principal_class() -> LeasePrincipalClass {
    if orbit_common::governance::authorization::agent_context_declared() {
        LeasePrincipalClass::Agent
    } else {
        LeasePrincipalClass::Human
    }
}

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

/// The actor recorded on leases granted through the lease intake socket, and
/// the tool name of its audit rows.
const LEASE_INTAKE_ACTOR: &str = "lease-intake";

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

/// Refuse a token request or expansion that would leave a lease with more
/// than [`TOKEN_MAX_LEASE_SELECTORS`] selectors, before anything is written.
fn ensure_token_selector_count(count: usize) -> Result<(), OrbitError> {
    if count <= TOKEN_MAX_LEASE_SELECTORS {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "a lease token's lease may have at most {TOKEN_MAX_LEASE_SELECTORS} selectors; this \
         one would have {count}. Lease a covering `dir:` selector instead"
    )))
}

/// A timestamp at least as long as any the store writes, for sizing answers.
const PLACEHOLDER_TIMESTAMP: &str = "0000-00-00T00:00:00.000000000+00:00";

/// What remains of a lease-token answer's size limit for its holder lists
/// (`overlaps`, `conflicts`, `queued_ahead`), measured as serialized JSON.
/// The lists are built only to a prefix that fits, and `truncated` says so;
/// every holder still decides grants and queueing and gets its notice. SSH
/// and dashboard callers are unbounded.
struct ResponseBudget {
    remaining: Option<usize>,
    truncated: bool,
}

impl ResponseBudget {
    fn unbounded() -> Self {
        Self {
            remaining: None,
            truncated: false,
        }
    }

    /// The budget left by `fixed`, the largest answer the operation can give
    /// with empty holder lists. When that alone cannot fit `max_bytes` the
    /// operation is refused here, before it writes anything.
    fn for_token(max_bytes: usize, fixed: &Value) -> Result<Self, OrbitError> {
        let needed = serialized_len(fixed) + TOKEN_RESPONSE_MARGIN_BYTES;
        if needed > max_bytes {
            return Err(OrbitError::InvalidInput(format!(
                "the answer to this lease operation would need about {needed} bytes, over the \
                 {max_bytes}-byte limit of the lease token transport; use fewer or shorter \
                 selectors, tags or branch names"
            )));
        }
        Ok(Self {
            remaining: Some(max_bytes - needed),
            truncated: false,
        })
    }

    /// Whether `item` still fits; once one does not, nothing more does, so
    /// every list stays a prefix.
    fn admit(&mut self, item: &impl Serialize) -> bool {
        let Some(remaining) = self.remaining.as_mut() else {
            return true;
        };
        // One more byte for the separating comma.
        let size = serialized_len(item) + 1;
        if self.truncated || size > *remaining {
            self.truncated = true;
            return false;
        }
        *remaining -= size;
        true
    }

    /// The bytes left for the store to fill `conflicts` and `queued_ahead`
    /// with; `None` when unbounded.
    fn holder_list_bytes(&self) -> Option<usize> {
        self.remaining
            .map(|remaining| if self.truncated { 0 } else { remaining })
    }

    /// The most conflicts the rest of the budget could show, each costing at
    /// least an empty one; `None` when unbounded.
    fn conflicts_shown(&self) -> Option<usize> {
        let smallest = serialized_len(&TaskLockConflict {
            file: String::new(),
            held_by: TaskLockHolder::Task,
            held_by_id: String::new(),
        }) + 1;
        self.holder_list_bytes().map(|bytes| bytes / smallest)
    }

    fn take<T: Serialize>(&mut self, items: Vec<T>) -> Vec<T> {
        items
            .into_iter()
            .take_while(|item| self.admit(item))
            .collect()
    }
}

fn serialized_len(value: &impl Serialize) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX / 2, |bytes| bytes.len())
}

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

/// Refuse a renewal or expansion of a lease whose mode or priority exceeds
/// the calling token or its class policy, leaving the lease unchanged.
fn ensure_token_covers_lease(
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

/// One holder overlapping a checked or leased path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct LeaseHolder {
    /// The checked or leased selector the holder overlaps.
    file: String,
    held_by: HolderKind,
    held_by_id: String,
    /// The holder's selector that overlaps `file`.
    held_selector: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<LeaseMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    principal: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum HolderKind {
    Task,
    Reservation,
    Lease,
}

/// Every active path holder, read without mutating the store.
struct PathHolders<'a> {
    index: &'a TaskLockIndex,
    reservations: Vec<(String, Vec<String>)>,
    leases: Vec<LeaseRecord>,
}

impl<'a> PathHolders<'a> {
    fn load(
        runtime: &RuntimeKernel,
        index: &'a TaskLockIndex,
        exclude_lease: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let orbit_dir = workspace_orbit_dir(runtime);
        let workspace_id = workspace_task_reservation_id(runtime)?;
        let store = runtime.stores().task_reservations();
        let reservations = store
            .inspect_active_task_reservations(&orbit_dir, workspace_id.as_deref())?
            .into_iter()
            .filter(|reservation| reservation.kind == TaskReservationKind::TaskReservation)
            .map(|reservation| (reservation.reservation_id, reservation.files))
            .collect();
        let leases = store
            .inspect_active_leases(&orbit_dir, workspace_id.as_deref())?
            .into_iter()
            .filter(|lease| Some(lease.lease_id.as_str()) != exclude_lease)
            .collect();
        Ok(Self {
            index,
            reservations,
            leases,
        })
    }

    /// The holders overlapping any of `files`, tasks first (in lock-status
    /// order), then reservations, then leases (oldest first): the prefix that
    /// fits `budget`, built without materializing the rest.
    fn overlapping(
        &self,
        files: &[String],
        workspace_root: &Path,
        budget: &mut ResponseBudget,
    ) -> Vec<LeaseHolder> {
        let mut holders = Vec::new();
        let _ = self.visit_overlapping(files, workspace_root, &mut |holder| {
            if budget.admit(&holder) {
                holders.push(holder);
                ControlFlow::Continue(())
            } else {
                ControlFlow::Break(())
            }
        });
        holders
    }

    fn visit_overlapping(
        &self,
        files: &[String],
        workspace_root: &Path,
        visit: &mut dyn FnMut(LeaseHolder) -> ControlFlow<()>,
    ) -> ControlFlow<()> {
        let mut tasks = self
            .index
            .tasks()
            .filter(|task| holds_context_lock(task.status, &task.tags))
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| {
            (
                task_lock_status_rank(task.status),
                task.created_at,
                task.id.clone(),
            )
        });
        for task in tasks {
            let held = self.index.lock_context_files(task, workspace_root);
            visit_overlaps(visit, files, &held, |file, held_selector| LeaseHolder {
                file,
                held_by: HolderKind::Task,
                held_by_id: task.id.clone(),
                held_selector,
                mode: None,
                principal: None,
            })?;
        }
        for (reservation_id, held) in &self.reservations {
            visit_overlaps(visit, files, held, |file, held_selector| LeaseHolder {
                file,
                held_by: HolderKind::Reservation,
                held_by_id: reservation_id.clone(),
                held_selector,
                mode: None,
                principal: None,
            })?;
        }
        for lease in &self.leases {
            visit_overlaps(visit, files, &lease.selectors, |file, held_selector| {
                LeaseHolder {
                    file,
                    held_by: HolderKind::Lease,
                    held_by_id: lease.lease_id.clone(),
                    held_selector,
                    mode: Some(lease.mode),
                    principal: Some(lease.principal.clone()),
                }
            })?;
        }
        ControlFlow::Continue(())
    }
}

fn visit_overlaps(
    visit: &mut dyn FnMut(LeaseHolder) -> ControlFlow<()>,
    files: &[String],
    held: &[String],
    holder: impl Fn(String, String) -> LeaseHolder,
) -> ControlFlow<()> {
    for file in files {
        for held_selector in held {
            if overlaps(file, held_selector) {
                visit(holder(file.clone(), held_selector.clone()))?;
            }
        }
    }
    ControlFlow::Continue(())
}

/// Whether `selector` covers all of `path`: the same anchor, or a `dir:`
/// whose anchor contains it.
fn selector_covers(selector: &str, path: &str) -> bool {
    let (Some(held), Some(checked)) = (OverlapScope::parse(selector), OverlapScope::parse(path))
    else {
        return false;
    };
    let OverlapScope::Anchored { path: anchor, .. } = &checked else {
        return false;
    };
    held.overlap_point(&checked) == Some(anchor.as_str())
}

fn lease_active(lease: &LeaseRecord) -> bool {
    lease.released_at.is_none()
        && DateTime::parse_from_rfc3339(&lease.expires_at)
            .is_ok_and(|expires_at| expires_at.with_timezone(&Utc) > Utc::now())
}

/// A lease's fields plus its derived `state`: `queued` while a request
/// waits, `granted` while it holds its selectors (an expansion may still
/// wait in `queued_selectors`), then `released` or `expired`.
fn lease_json(lease: &LeaseRecord) -> Value {
    let state = match (&lease.released_at, lease.release_reason) {
        (None, _) if lease_active(lease) && lease.queue_kind == Some(LeaseQueueKind::Request) => {
            "queued"
        }
        (None, _) if lease_active(lease) => "granted",
        (None, _) | (Some(_), Some(TaskReservationReleaseReason::TtlExpired)) => "expired",
        (Some(_), _) => "released",
    };
    let mut value = json!(lease);
    value["state"] = json!(state);
    value
}

fn parse_selectors(
    input: &Value,
    keys: &[&str],
    field: &str,
    workspace_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    let raw = optional_string_list_alias(input, keys)?
        .ok_or_else(|| OrbitError::InvalidInput(format!("missing `{field}`")))?;
    canonicalize_file_lock_selectors(
        &parse_file_lock_selectors(raw, field)?,
        field,
        workspace_root,
    )
}

fn required_non_empty(input: &Value, field: &str) -> Result<String, OrbitError> {
    let value = required_string(input, &[field], field)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(OrbitError::InvalidInput(format!(
            "`{field}` must not be empty"
        )));
    }
    Ok(value.to_string())
}

fn required_lease_id(input: &Value) -> Result<String, OrbitError> {
    let lease_id = required_string(input, &["lease_id", "leaseId", "lease-id"], "lease_id")?;
    validate_lease_id_form(&lease_id)?;
    Ok(lease_id)
}

/// Lease ids are minted as `lease-<id>`; a reservation or task id can never
/// match one, so it is refused rather than answered as "not found".
fn validate_lease_id_form(lease_id: &str) -> Result<(), OrbitError> {
    if lease_id.starts_with(LEASE_ID_PREFIX) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "`lease_id` must have the form `{LEASE_ID_PREFIX}<id>`; got `{lease_id}`"
    )))
}

fn parse_mode(raw: &str, field: &str) -> Result<LeaseMode, OrbitError> {
    serde_json::from_value(json!(raw.trim())).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "`{field}` must be `advisory` or `hard`; got `{raw}`"
        ))
    })
}

fn parse_priority(raw: &str, field: &str) -> Result<LeasePriority, OrbitError> {
    serde_json::from_value(json!(raw.trim())).map_err(|_| {
        OrbitError::InvalidInput(format!(
            "`{field}` must be `low`, `normal` or `high`; got `{raw}`"
        ))
    })
}

fn hard_allowed(raw: &str, key: &str) -> Result<bool, OrbitError> {
    match raw {
        "allow" => Ok(true),
        "never" => Ok(false),
        other => Err(OrbitError::InvalidInput(format!(
            "`{key}` must be `allow` or `never`; got `{other}`"
        ))),
    }
}

fn priority_rank(priority: LeasePriority) -> u8 {
    match priority {
        LeasePriority::Low => 0,
        LeasePriority::Normal => 1,
        LeasePriority::High => 2,
    }
}

fn lower_priority(left: LeasePriority, right: LeasePriority) -> LeasePriority {
    if priority_rank(left) <= priority_rank(right) {
        left
    } else {
        right
    }
}
