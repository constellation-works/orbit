//! Task reservation commands and input validation.

use std::collections::BTreeSet;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::{Selector, canonical_selector_in_workspace};
use orbit_common::protocol::tool_input::{
    optional_string_list_alias, optional_u32_alias, required_string,
};
use orbit_store::contracts::{
    TaskLockConflict, TaskReservationCheckParams, TaskReservationReleaseParams,
    TaskReservationReleaseReason, TaskReservationReserveParams,
};
use orbit_tools::ReservationOwnerContext;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use super::audit::{emit_expired_reservation_events, first_task_id, record_task_lock_audit_event};
use super::index::{
    TaskLockIndex, context_lock_exempt, invalid_declared_selectors, merge_task_lock_conflicts,
    requested_task_files_indexed, task_lock_conflicts_indexed,
};
use crate::RuntimeKernel;

/// The in-process reservation answer. JSON is produced only for a tool or
/// persisted step output; ordinary grants omit the deferred-footprint keys.
pub enum ReservationOutcome {
    Granted {
        reservation_id: String,
        expires_at: Option<String>,
        reserved_files: Vec<String>,
        footprint_deferred: bool,
        deferred_conflicts: Vec<TaskLockConflict>,
    },
    Denied {
        conflicts: Vec<TaskLockConflict>,
    },
}

impl ReservationOutcome {
    pub(crate) fn into_tool_output(self) -> Value {
        match self {
            Self::Granted {
                reservation_id,
                expires_at,
                reserved_files,
                footprint_deferred,
                deferred_conflicts,
            } => {
                let mut output = json!({
                    "reserved": true,
                    "reservation_id": reservation_id,
                    "expires_at": expires_at,
                    "reserved_files": reserved_files,
                });
                if footprint_deferred {
                    output["footprint_deferred"] = json!(true);
                    output["deferred_conflicts"] = json!(deferred_conflicts);
                }
                output
            }
            Self::Denied { conflicts } => json!({"reserved": false, "conflicts": conflicts}),
        }
    }

    /// The gate always publishes defaults for its template-visible fields.
    pub fn into_step_output(self, waiting_on_deps: Vec<String>) -> Value {
        let footprint_deferred = matches!(
            self,
            Self::Granted {
                footprint_deferred: true,
                ..
            }
        );
        let granted = matches!(self, Self::Granted { .. });
        let mut output = self.into_tool_output();
        output["waiting_on_deps"] = json!(waiting_on_deps);
        output["footprint_deferred"] = json!(footprint_deferred);
        if granted {
            output["conflicts"] = json!([]);
        }
        output
    }
}

const MAX_TASK_RESERVATION_TTL_SECONDS: u32 = 14400;

pub fn list(runtime: &RuntimeKernel) -> Result<Value, OrbitError> {
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let reservation_result = runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(&workspace_orbit_dir(runtime), workspace_id.as_deref())?;
    emit_expired_reservation_events(runtime, &reservation_result.expired_reservations)?;

    // Expand each task's lock surface once and reuse it for both projections
    // below. The expansion canonicalizes every declared selector, so computing
    // it per projection doubled the work for a listing that has a single
    // answer.
    let repo_root = runtime.paths().repo_root.as_path();
    let locked_surfaces = TaskLockIndex::load(runtime, &[])?.into_active_lock_surfaces(repo_root);

    Ok(super::projection::lock_listing_json(
        &locked_surfaces,
        &reservation_result.reservations,
    ))
}

pub fn release(
    runtime: &RuntimeKernel,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let reservation_id = required_string(
        &input,
        &["reservation_id", "reservationId", "reservation-id"],
        "reservation_id",
    )?;
    validate_reservation_id_form(&reservation_id)?;
    let result = runtime
        .stores()
        .task_reservations()
        .release_task_reservation(TaskReservationReleaseParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime)?,
            reservation_id: reservation_id.clone(),
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: Some(
                json!({
                    "released_by": reservation_actor_label(
                        runtime,
                        agent.as_deref(),
                        model.as_deref(),
                    )?,
                })
                .to_string(),
            ),
        })?;
    emit_expired_reservation_events(runtime, &result.expired_reservations)?;
    if result.released {
        let released_task_id = result
            .reservation
            .as_ref()
            .and_then(|reservation| first_task_id(&reservation.task_ids));
        let owner_run_id = result
            .reservation
            .as_ref()
            .and_then(|reservation| reservation.owner_run_id.clone());
        record_task_lock_audit_event(
            runtime,
            "task.locks.reserve.released",
            "orbit.task.locks.release",
            Some(reservation_id.as_str()),
            released_task_id,
            AuditEventStatus::Success,
            json!({
                "reservation_id": reservation_id,
                "owner_run_id": owner_run_id,
                "release_reason": TaskReservationReleaseReason::Explicit.as_str(),
                "released_at": result.released_at,
                "released_by": reservation_actor_label(
                    runtime,
                    agent.as_deref(),
                    model.as_deref(),
                )?,
            }),
        )?;
    }
    Ok(json!({ "released": result.released }))
}

/// Reservation ids are minted as `reservation-<nanos>`
/// (`TaskReservationStoreBackend::reserve_task_reservation`). A
/// task id or other identifier passed here can never match a stored
/// reservation, so without this check `release` falls through to the "no
/// matching row" path and returns a falsy `{"released": false}` — indistinguishable
/// from a completed release.
fn validate_reservation_id_form(reservation_id: &str) -> Result<(), OrbitError> {
    const RESERVATION_ID_PREFIX: &str = "reservation-";
    if reservation_id.starts_with(RESERVATION_ID_PREFIX) {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "`reservation_id` must have the form `{RESERVATION_ID_PREFIX}<id>` (see `orbit task locks list --json`); got `{reservation_id}`, which does not look like a reservation id"
    )))
}

pub fn reserve(
    runtime: &RuntimeKernel,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    reservation_owner: Option<ReservationOwnerContext>,
) -> Result<Value, OrbitError> {
    let requested_task_ids = match parse_task_lock_reservation_scope(&input)? {
        TaskLockReservationScope::TaskIds(task_ids) => task_ids,
        TaskLockReservationScope::Files(_) => Vec::new(),
    };
    let index = TaskLockIndex::load(runtime, &requested_task_ids)?;
    reserve_with_index(
        runtime,
        input,
        agent,
        model,
        reservation_owner,
        &index,
        EmptyTaskSurfacePolicy::Refuse,
    )
    .map(ReservationOutcome::into_tool_output)
}

/// Whether a task-scope reservation that resolves to zero files is a mistake
/// to refuse, or a legitimate no-op to admit.
///
/// An operator claiming a task's surface before starting work almost
/// certainly wants a real claim: a task with nothing declared should be told
/// so, not handed a reservation ID that holds nothing ([`Self::Refuse`]). The
/// legacy v2 dispatch admission gate uses a compatibility no-op instead: a
/// task that has not declared any context yet has nothing to serialize
/// against, so admitting it trivially is correct there ([`Self::Admit`]).
/// Distributed pull admission likewise admits empty footprints, preserving
/// claim identity without holding a context lock.
///
/// Neither value reaches an inherited-only `epic` root: that refusal is decided
/// ahead of this policy, because the surface such a root is missing is one its
/// descendants used to supply rather than one nobody in the family ever had.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmptyTaskSurfacePolicy {
    Refuse,
    Admit,
}

/// Reserve a task-lock scope using an index loaded by the calling operation.
///
/// The v2 `reserve_locks` action needs the same task index for its one-poll
/// admission path and its reservation check. Keeping the actual reservation
/// behavior here gives both callers one canonical implementation while letting
/// that action avoid reloading every task bundle.
pub fn reserve_with_index(
    runtime: &RuntimeKernel,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    reservation_owner: Option<ReservationOwnerContext>,
    index: &TaskLockIndex,
    empty_task_surface_policy: EmptyTaskSurfacePolicy,
) -> Result<ReservationOutcome, OrbitError> {
    let reservation_scope = parse_task_lock_reservation_scope(&input)?;
    let ttl_seconds =
        optional_u32_alias(&input, &["ttl_seconds", "ttlSeconds", "ttl-seconds"])?.unwrap_or(1800);
    if !(1..=MAX_TASK_RESERVATION_TTL_SECONDS).contains(&ttl_seconds) {
        return Err(OrbitError::InvalidInput(format!(
            "`ttl_seconds` must be between 1 and {MAX_TASK_RESERVATION_TTL_SECONDS} seconds"
        )));
    }

    let actor = reservation_actor_label(runtime, agent.as_deref(), model.as_deref())?;
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let repo_root = runtime.paths().repo_root.as_path();
    let (task_ids, requested_files) = match &reservation_scope {
        TaskLockReservationScope::TaskIds(task_ids) => {
            // Validate every id exists before judging whether the bundle
            // declares a surface, so an unknown task id is still reported as
            // not-found rather than folded into this refusal.
            let requested_files = requested_task_files_indexed(index, task_ids, repo_root)?;
            // Ahead of the policy branch, and so refused on every entry point:
            // an `epic`-tagged root that declared nothing of its own no longer
            // inherits the descendant surface it relied on, and the
            // compatibility no-op that admits an ordinary undeclared task would
            // otherwise hand exactly the task defined as taken on *whole* a
            // reservation holding nothing.
            if let Some(task_id) = task_ids
                .iter()
                .find(|task_id| index.is_inherited_only_epic_root(task_id))
            {
                return Err(inherited_only_epic_root_error(task_id));
            }
            if empty_task_surface_policy == EmptyTaskSurfacePolicy::Refuse {
                if task_ids
                    .iter()
                    .all(|task_id| !index.declares_context_surface(task_id))
                {
                    return Err(no_lock_surface_error(task_ids));
                }
                // Reached only when every declaration failed
                // canonicalization: a declared-but-not-yet-created target
                // keeps its selector, so an empty surface here is an invalid
                // declaration, not a missing file.
                if requested_files.is_empty() {
                    return Err(invalid_lock_surface_error(
                        task_ids,
                        &invalid_declared_selectors(index, task_ids, repo_root),
                    ));
                }
            }
            (task_ids.clone(), requested_files)
        }
        TaskLockReservationScope::Files(files) => (
            Vec::new(),
            canonicalize_file_lock_selectors(files, repo_root)?,
        ),
    };
    runtime.reconcile_stale_owned_reservations_for_files(&requested_files, 32)?;
    let mut conflicts = task_lock_conflicts_indexed(index, &task_ids, &requested_files, repo_root);
    // A `no-diff-expected` scope still checks its original footprint against
    // persistent holders atomically at the store boundary. The grant holds
    // no files: `release_locks` keeps a
    // reservation id, and the row cannot block a later overlapping task
    // [ORB-14247].
    let stored_files =
        if conflicts.is_empty() && reservation_context_lock_exempt(index, &reservation_scope) {
            Vec::new()
        } else {
            requested_files.clone()
        };

    record_task_lock_audit_event(
        runtime,
        "task.locks.reserve.requested",
        "orbit.task.locks.reserve",
        None,
        first_task_id(&task_ids),
        AuditEventStatus::Success,
        json!({
            "actor": actor.clone(),
            "task_ids": task_ids.clone(),
            "files": requested_files.clone(),
            "ttl_seconds": ttl_seconds,
            "owner_run_id": reservation_owner
                .as_ref()
                .map(|owner| owner.owner_run_id.clone()),
        }),
    )?;

    let reservation_result = if conflicts.is_empty() {
        runtime
            .stores()
            .task_reservations()
            .reserve_task_reservation(TaskReservationReserveParams {
                workspace_orbit_dir: workspace_orbit_dir(runtime),
                workspace_id: workspace_id.clone(),
                task_ids: task_ids.clone(),
                requested_files: requested_files.clone(),
                stored_files,
                actor: actor.clone(),
                ttl_seconds,
                owner_run_id: reservation_owner
                    .as_ref()
                    .map(|owner| owner.owner_run_id.clone()),
                owner_metadata_json: reservation_owner
                    .as_ref()
                    .and_then(|owner| owner.owner_metadata_json.clone()),
            })?
    } else {
        let check = runtime
            .stores()
            .task_reservations()
            .check_task_reservation_conflicts(TaskReservationCheckParams {
                workspace_orbit_dir: workspace_orbit_dir(runtime),
                workspace_id: workspace_id.clone(),
                requested_files: requested_files.clone(),
            })?;
        conflicts = merge_task_lock_conflicts(conflicts, check.conflicts);
        emit_expired_reservation_events(runtime, &check.expired_reservations)?;
        orbit_store::contracts::TaskReservationReserveResult {
            reserved: false,
            reservation_id: None,
            expires_at: None,
            reserved_files: Vec::new(),
            conflicts: conflicts.clone(),
            expired_reservations: Vec::new(),
        }
    };

    emit_expired_reservation_events(runtime, &reservation_result.expired_reservations)?;

    if reservation_result.reserved {
        let reservation_id = reservation_result.reservation_id.clone().ok_or_else(|| {
            OrbitError::Execution("reservation grant is missing reservation_id".to_string())
        })?;
        record_task_lock_audit_event(
            runtime,
            "task.locks.reserve.granted",
            "orbit.task.locks.reserve",
            Some(reservation_id.as_str()),
            first_task_id(&task_ids),
            AuditEventStatus::Success,
            json!({
                "reservation_id": reservation_id,
                "files": reservation_result.reserved_files.clone(),
                "expires_at": reservation_result.expires_at.clone(),
                "actor": actor,
                "task_ids": task_ids.clone(),
                "owner_run_id": reservation_owner
                    .as_ref()
                    .map(|owner| owner.owner_run_id.clone()),
            }),
        )?;
        Ok(ReservationOutcome::Granted {
            reservation_id,
            expires_at: reservation_result.expires_at,
            reserved_files: reservation_result.reserved_files,
            footprint_deferred: false,
            deferred_conflicts: Vec::new(),
        })
    } else {
        let conflicts = merge_task_lock_conflicts(conflicts, reservation_result.conflicts);
        record_task_lock_audit_event(
            runtime,
            "task.locks.reserve.denied",
            "orbit.task.locks.reserve",
            None,
            first_task_id(&task_ids),
            AuditEventStatus::Denied,
            json!({
                "actor": actor,
                "task_ids": task_ids.clone(),
                "files": requested_files.clone(),
                "conflicts": conflicts.clone(),
                "owner_run_id": reservation_owner
                    .as_ref()
                    .map(|owner| owner.owner_run_id.clone()),
            }),
        )?;
        Ok(ReservationOutcome::Denied { conflicts })
    }
}

/// Grant `task_ids` a reservation that holds no files, for a delivery that
/// takes its footprint only when it first edits: a resumed published
/// delivery, which adopts its existing branch and pull request without
/// touching the working tree. `deferred_conflicts` are the holders an ordinary
/// reservation was refused on; the grant names them rather than waiting.
///
/// The row keeps the reservation id the gate releases, and, holding no files,
/// cannot block an overlapping task. The leaf takes the files itself before
/// its first agent edit (`acquire_deferred_footprint`).
pub fn reserve_deferred_footprint(
    runtime: &RuntimeKernel,
    input: &Value,
    agent: Option<String>,
    model: Option<String>,
    reservation_owner: Option<ReservationOwnerContext>,
    deferred_conflicts: Vec<TaskLockConflict>,
) -> Result<ReservationOutcome, OrbitError> {
    let task_ids = parse_task_ids(input)?;
    let ttl_seconds =
        optional_u32_alias(input, &["ttl_seconds", "ttlSeconds", "ttl-seconds"])?.unwrap_or(1800);
    if !(1..=MAX_TASK_RESERVATION_TTL_SECONDS).contains(&ttl_seconds) {
        return Err(OrbitError::InvalidInput(format!(
            "`ttl_seconds` must be between 1 and {MAX_TASK_RESERVATION_TTL_SECONDS} seconds"
        )));
    }
    let actor = reservation_actor_label(runtime, agent.as_deref(), model.as_deref())?;
    let owner_run_id = reservation_owner
        .as_ref()
        .map(|owner| owner.owner_run_id.clone());
    let result = runtime
        .stores()
        .task_reservations()
        .reserve_task_reservation(TaskReservationReserveParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime)?,
            task_ids: task_ids.clone(),
            requested_files: Vec::new(),
            stored_files: Vec::new(),
            actor: actor.clone(),
            ttl_seconds,
            owner_run_id: owner_run_id.clone(),
            owner_metadata_json: reservation_owner.and_then(|owner| owner.owner_metadata_json),
        })?;
    emit_expired_reservation_events(runtime, &result.expired_reservations)?;
    let reservation_id = result.reservation_id.clone().ok_or_else(|| {
        OrbitError::Execution("reservation grant is missing reservation_id".to_string())
    })?;
    record_task_lock_audit_event(
        runtime,
        "task.locks.reserve.granted",
        "orbit.task.locks.reserve",
        Some(reservation_id.as_str()),
        first_task_id(&task_ids),
        AuditEventStatus::Success,
        json!({
            "reservation_id": reservation_id,
            "files": [],
            "expires_at": result.expires_at.clone(),
            "actor": actor,
            "task_ids": task_ids,
            "owner_run_id": owner_run_id,
            "footprint_deferred": true,
            "deferred_conflicts": deferred_conflicts.clone(),
        }),
    )?;
    Ok(ReservationOutcome::Granted {
        reservation_id,
        expires_at: result.expires_at,
        reserved_files: Vec::new(),
        footprint_deferred: true,
        deferred_conflicts,
    })
}

/// The runs that own an active reservation holding files.
pub fn runs_holding_file_reservations(
    runtime: &RuntimeKernel,
) -> Result<BTreeSet<String>, OrbitError> {
    let listed = runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(
            &workspace_orbit_dir(runtime),
            workspace_task_reservation_id(runtime)?.as_deref(),
        )?;
    emit_expired_reservation_events(runtime, &listed.expired_reservations)?;
    Ok(listed
        .reservations
        .into_iter()
        .filter(|reservation| !reservation.files.is_empty())
        .filter_map(|reservation| reservation.owner_run_id)
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TaskLockReservationScope {
    TaskIds(Vec<String>),
    Files(Vec<String>),
}

fn parse_task_lock_reservation_scope(
    input: &Value,
) -> Result<TaskLockReservationScope, OrbitError> {
    let task_ids = optional_string_list_alias(input, &["task_ids", "taskIds", "task-ids"])?;
    let files = optional_string_list_alias(input, &["files"])?;

    match (task_ids, files) {
        (Some(_), Some(_)) | (None, None) => Err(OrbitError::InvalidInput(
            "exactly one of 'task_ids' or 'files' must be provided".to_string(),
        )),
        (Some(task_ids), None) => {
            parse_task_id_list(task_ids).map(TaskLockReservationScope::TaskIds)
        }
        (None, Some(files)) => {
            parse_file_lock_selectors(files).map(TaskLockReservationScope::Files)
        }
    }
}

pub fn parse_task_ids(input: &Value) -> Result<Vec<String>, OrbitError> {
    let task_ids = optional_string_list_alias(input, &["task_ids", "taskIds", "task-ids"])?
        .ok_or_else(|| OrbitError::InvalidInput("missing `task_ids`".to_string()))?;
    parse_task_id_list(task_ids)
}

fn parse_task_id_list(task_ids: Vec<String>) -> Result<Vec<String>, OrbitError> {
    let deduped = task_ids.into_iter().collect::<BTreeSet<_>>();
    if deduped.is_empty() {
        return Err(OrbitError::InvalidInput(
            "`task_ids` must contain at least one task ID".to_string(),
        ));
    }
    Ok(deduped.into_iter().collect())
}

fn parse_file_lock_selectors(files: Vec<String>) -> Result<Vec<String>, OrbitError> {
    let mut deduped = BTreeSet::new();
    for raw in files {
        let selector: Selector = raw.parse().map_err(|error| {
            OrbitError::InvalidInput(format!(
                "`files` entries must be canonical file or directory selectors using `file:` or `dir:`: {error}"
            ))
        })?;
        match &selector {
            Selector::Dir { .. } | Selector::File { .. } => {
                deduped.insert(selector.to_string());
            }
            Selector::Symbol { .. } | Selector::Module { .. } | Selector::Command { .. } => {
                return Err(OrbitError::InvalidInput(
                    "`files` entries must be canonical file or directory selectors using `file:` or `dir:`; `symbol:`, `module:`, and `command:` selectors are not supported for task locks".to_string(),
                ));
            }
        }
    }
    if deduped.is_empty() {
        return Err(OrbitError::InvalidInput(
            "`files` must contain at least one file or directory selector using `file:` or `dir:`"
                .to_string(),
        ));
    }
    Ok(deduped.into_iter().collect())
}

fn canonicalize_file_lock_selectors(
    files: &[String],
    workspace_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    files
        .iter()
        .map(|selector| {
            canonical_selector_in_workspace(selector, workspace_root).map_err(|error| {
                OrbitError::InvalidInput(format!(
                    "`files` entries must remain inside workspace `{}`: {error}",
                    workspace_root.display()
                ))
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()
        .map(|selectors| selectors.into_iter().collect())
}

pub fn workspace_orbit_dir(runtime: &RuntimeKernel) -> String {
    runtime.paths().orbit_dir.to_string_lossy().into_owned()
}

pub fn workspace_task_reservation_id(
    runtime: &RuntimeKernel,
) -> Result<Option<String>, OrbitError> {
    runtime.workspace_id().map(Some)
}

/// A task-id reservation holds nothing when every named task is
/// `no-diff-expected`. An explicit `files` reservation is an operator lock
/// and is never exempt. A missing envelope is not exempt: the caller has
/// already rejected unknown ids, and a gap must not drop a real surface.
fn reservation_context_lock_exempt(
    index: &TaskLockIndex,
    scope: &TaskLockReservationScope,
) -> bool {
    let TaskLockReservationScope::TaskIds(task_ids) = scope else {
        return false;
    };
    !task_ids.is_empty()
        && task_ids.iter().all(|task_id| {
            index
                .get(task_id)
                .is_some_and(|task| context_lock_exempt(&task.tags))
        })
}

fn reservation_actor_label(
    runtime: &RuntimeKernel,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<String, OrbitError> {
    runtime.actor().resolve_write_label(agent, model)
}

/// A bundle that declares context whose every selector is unusable holds no
/// files either, but for a different reason than
/// [`no_lock_surface_error`]: the declaration exists and needs correcting
/// rather than supplying. Naming the offending selectors is what makes the
/// pre-admission repair actionable instead of a guess.
fn invalid_lock_surface_error(task_ids: &[String], invalid: &[String]) -> OrbitError {
    let (subject, verb) = if task_ids.len() == 1 {
        ("task", "declares")
    } else {
        ("tasks", "declare")
    };
    let listed = if invalid.is_empty() {
        "none could be canonicalized".to_string()
    } else {
        invalid.join(", ")
    };
    OrbitError::InvalidInput(format!(
        "{subject} {} {verb} no usable context surface: no declared selector canonicalizes \
         against this workspace ({listed}). Declared targets that do not exist yet are kept, so \
         repair the invalid selectors with `orbit task update --context` before reserving",
        task_ids.join(", ")
    ))
}

fn no_lock_surface_error(task_ids: &[String]) -> OrbitError {
    let (subject, verb) = if task_ids.len() == 1 {
        ("task", "declares")
    } else {
        ("tasks", "declare")
    };
    OrbitError::InvalidInput(format!(
        "{subject} {} {verb} no context surface to reserve (no `context_files` declared); \
         nothing would be locked. Add context with `orbit task update --context`, or reserve \
         explicit selectors with `--file` instead",
        task_ids.join(", ")
    ))
}

/// The refusal an inherited-only `epic` root gets, naming both repairs the
/// design admits: declare the root's own surface, or retire the root.
fn inherited_only_epic_root_error(task_id: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "task {task_id} carries the `epic` size tag and declares no `context_files` of its own, \
         while its descendants do; a root no longer inherits its descendants' surface, so \
         reserving it would hold nothing while that work runs beside it. Declare its own surface \
         with `orbit task update --context`, or retire the root"
    ))
}
