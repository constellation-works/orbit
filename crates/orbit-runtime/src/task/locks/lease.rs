//! Path-lease operations (`docs/design/path-leases`, phase 1): request,
//! renew, release, show, list and check over the lease rows the reservation
//! store keeps.
//!
//! A lease is held by a principal (a person or an agent), never by a task.
//! Advisory leases are always granted and block nothing; a hard lease is
//! granted only when no selector of its hard surface overlaps a task context
//! lock, a task reservation or another hard lease. There is no waiting queue
//! yet: an overlapping hard request is refused with the holders named.
//!
//! Over SSH the principal class and its `lease.*` ceilings are attribution
//! plus a guard, not a security boundary.

use std::path::Path;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::fs::selector::{OverlapScope, overlaps};
use orbit_common::protocol::tool_input::{
    optional_string_alias, optional_string_list_alias, required_string,
};
use orbit_store::contracts::{
    LeaseInsertParams, LeaseMode, LeaseRecord, LeaseReleaseParams, LeaseRenewParams,
    TaskLockConflict, TaskReservationCheckParams, TaskReservationKind,
    TaskReservationReleaseReason,
};
pub use orbit_store::contracts::{LeasePrincipalClass, LeasePriority};
use orbit_types::telemetry::AuditEventStatus;
use serde::Serialize;
use serde_json::{Value, json};

use super::audit::{
    emit_expired_lease_events, emit_expired_reservation_events, record_lease_audit_event,
};
use super::commands::{
    canonicalize_file_lock_selectors, parse_file_lock_selectors, parse_ttl_seconds,
    reservation_actor_label, workspace_orbit_dir, workspace_task_reservation_id,
};
use super::index::{
    TaskLockIndex, holds_context_lock, merge_task_lock_conflicts, task_lock_conflicts_indexed,
    task_lock_status_rank,
};
use crate::RuntimeKernel;

const REQUEST_TOOL: &str = "orbit.lease.request";
const RENEW_TOOL: &str = "orbit.lease.renew";
const RELEASE_TOOL: &str = "orbit.lease.release";
const SHOW_TOOL: &str = "orbit.lease.show";
const LIST_TOOL: &str = "orbit.lease.list";
const LEASE_ID_PREFIX: &str = "lease-";

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
/// Advisory is always granted and names the holders it overlaps. Hard drops
/// `workflow.unlocked_context_paths` from its surface and is granted only
/// when the rest overlaps no task context lock, task reservation or hard
/// lease; otherwise it is refused with those holders. A class whose
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
}

/// Request a lease for an authenticated lease token.
///
/// The same operation as [`request`], with the token's principal and class
/// instead of the caller's declared principal and process class, and with the
/// stricter of the token's scope and the workspace's class policy: the lower
/// of the two priority ceilings, and hard only when both allow it. The lease
/// records the token id, and its audit rows name the token and client.
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

    let index = TaskLockIndex::load(runtime, &[])?;
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let task_conflicts = if mode == LeaseMode::Hard {
        runtime.reconcile_stale_owned_reservations_for_files(&stored_selectors, 32)?;
        task_lock_conflicts_indexed(&index, &[], &stored_selectors, repo_root)
    } else {
        Vec::new()
    };
    if !task_conflicts.is_empty() {
        // Name every holder, not only the tasks: the store reports the task
        // reservations and hard leases on the same surface.
        let check = runtime
            .stores()
            .task_reservations()
            .check_task_reservation_conflicts(TaskReservationCheckParams {
                workspace_orbit_dir: workspace_orbit_dir(runtime),
                workspace_id: workspace_id.clone(),
                requested_files: stored_selectors.clone(),
            })?;
        emit_expired_reservation_events(runtime, &check.expired_reservations)?;
        let conflicts = merge_task_lock_conflicts(task_conflicts, check.conflicts);
        return refuse(runtime, audit_request, &stored_selectors, conflicts);
    }

    let result = runtime
        .stores()
        .task_reservations()
        .insert_lease(LeaseInsertParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id,
            selectors: stored_selectors.clone(),
            mode,
            priority,
            tags: optional_string_list_alias(&input, &["tags"])?.unwrap_or_default(),
            principal: principal.clone(),
            principal_class,
            machine_id: optional_string_alias(&input, &["machine_id", "remote_caller_machine_id"])?,
            base_branch: Some(
                optional_string_alias(&input, &["base_branch", "base"])?
                    .unwrap_or_else(|| runtime.workspace_base_branch().to_string()),
            ),
            bound_branch: optional_string_alias(&input, &["branch", "bound_branch"])?,
            actor: actor.clone(),
            ttl_seconds,
            token_id: token.map(|grant| grant.token_id.clone()),
        })?;
    emit_expired_lease_events(runtime, REQUEST_TOOL, &result.expired_leases)?;
    let Some(lease) = result.lease else {
        return refuse(runtime, audit_request, &stored_selectors, result.conflicts);
    };

    let overlaps = if mode == LeaseMode::Advisory {
        PathHolders::load(runtime, &index, Some(&lease.lease_id))?
            .overlapping(&lease.selectors, repo_root)
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
) -> Result<Value, OrbitError> {
    payload["reason"] = json!("held");
    payload["hard_surface"] = json!(hard_surface);
    payload["conflicts"] = json!(conflicts);
    record_lease_audit_event(
        runtime,
        "lease.request.refused",
        REQUEST_TOOL,
        None,
        AuditEventStatus::Denied,
        payload,
    )?;
    Ok(json!({"granted": false, "conflicts": conflicts}))
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
    let lease = leases
        .get_lease(&orbit_dir, workspace_id.as_deref(), &lease_id)?
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!("no lease `{lease_id}` in this workspace"))
        })?;
    Ok(json!({"lease": lease_json(&lease)}))
}

/// The workspace's active leases, advisory and hard, oldest first.
pub fn list(runtime: &RuntimeKernel) -> Result<Value, OrbitError> {
    runtime.lease_policy().ensure_enabled()?;
    let listed = runtime.stores().task_reservations().list_active_leases(
        &workspace_orbit_dir(runtime),
        workspace_task_reservation_id(runtime)?.as_deref(),
    )?;
    emit_expired_lease_events(runtime, LIST_TOOL, &listed.expired_leases)?;
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
                "holders": holders.overlapping(std::slice::from_ref(path), repo_root),
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
    /// order), then reservations, then leases (oldest first).
    fn overlapping(&self, files: &[String], workspace_root: &Path) -> Vec<LeaseHolder> {
        let mut holders = Vec::new();
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
            push_overlaps(&mut holders, files, &held, |file, held_selector| {
                LeaseHolder {
                    file,
                    held_by: HolderKind::Task,
                    held_by_id: task.id.clone(),
                    held_selector,
                    mode: None,
                    principal: None,
                }
            });
        }
        for (reservation_id, held) in &self.reservations {
            push_overlaps(&mut holders, files, held, |file, held_selector| {
                LeaseHolder {
                    file,
                    held_by: HolderKind::Reservation,
                    held_by_id: reservation_id.clone(),
                    held_selector,
                    mode: None,
                    principal: None,
                }
            });
        }
        for lease in &self.leases {
            push_overlaps(
                &mut holders,
                files,
                &lease.selectors,
                |file, held_selector| LeaseHolder {
                    file,
                    held_by: HolderKind::Lease,
                    held_by_id: lease.lease_id.clone(),
                    held_selector,
                    mode: Some(lease.mode),
                    principal: Some(lease.principal.clone()),
                },
            );
        }
        holders
    }
}

fn push_overlaps(
    holders: &mut Vec<LeaseHolder>,
    files: &[String],
    held: &[String],
    holder: impl Fn(String, String) -> LeaseHolder,
) {
    for file in files {
        for held_selector in held {
            if overlaps(file, held_selector) {
                holders.push(holder(file.clone(), held_selector.clone()));
            }
        }
    }
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

/// A lease's fields plus its derived `state`.
fn lease_json(lease: &LeaseRecord) -> Value {
    let state = match (&lease.released_at, lease.release_reason) {
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
