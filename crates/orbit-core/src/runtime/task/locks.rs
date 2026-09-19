//! Task reservation and file-lock operations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_common::fs::selector::{Selector, canonical_selector_in_workspace};
use orbit_common::protocol::tool_input::{
    optional_string_list_alias, optional_u32_alias, required_string,
};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::contracts::{
    ExpiredTaskReservation, ReleasedTaskReservation, TaskLockConflict, TaskLockHolder,
    TaskReservationCheckParams, TaskReservationReleaseParams, TaskReservationReleaseReason,
    TaskReservationReserveParams,
};
use orbit_store::maintenance::task_registry::read_workspace_config_optional;
use orbit_tools::ReservationOwnerContext;
use orbit_types::task::{Task, TaskEnvelopeV2, TaskRelationType, TaskStatus};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::coordination_audit::{CoordinationAuditEvent, record_coordination_audit_event};
use crate::runtime::task::{DeclaredContextFiles, declared_context_files};

pub(crate) const MAX_TASK_RESERVATION_TTL_SECONDS: u32 = 14400;

pub(crate) fn list(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
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

    let locked_files: BTreeSet<String> = locked_surfaces
        .iter()
        .flat_map(|(_, files)| files.iter().cloned())
        .chain(
            reservation_result
                .reservations
                .iter()
                .flat_map(|reservation| reservation.files.iter().cloned()),
        )
        .collect();
    let by_reservation = reservation_result
        .reservations
        .iter()
        .map(|reservation| {
            json!({
                "reservation_id": reservation.reservation_id.clone(),
                "workspace_id": reservation.workspace_id.clone(),
                "task_ids": reservation.task_ids.clone(),
                "files": reservation.files.clone(),
                "actor": reservation.actor.clone(),
                "created_at": reservation.created_at.clone(),
                "expires_at": reservation.expires_at.clone(),
                "owner_run_id": reservation.owner_run_id.clone(),
                "owner_metadata_json": reservation.owner_metadata_json.clone(),
            })
        })
        .collect::<Vec<_>>();

    // Counts distinct task IDs across both projections: a task-bound
    // reservation names task IDs that need not belong to any active task (the
    // reserving task may still be `backlog`), so counting only
    // `locked_surfaces` undercounts whenever a reservation is the sole holder
    // for a task.
    let distinct_tasks: BTreeSet<&str> = locked_surfaces
        .iter()
        .map(|(task, _)| task.id.as_str())
        .chain(
            reservation_result
                .reservations
                .iter()
                .flat_map(|reservation| reservation.task_ids.iter().map(String::as_str)),
        )
        .collect();

    Ok(json!({
        "locked_files": locked_files.iter().cloned().collect::<Vec<_>>(),
        "by_task": locked_surfaces
            .iter()
            .map(|(task, files)| task_lock_to_json(task, files.clone()))
            .collect::<Vec<_>>(),
        "by_reservation": by_reservation,
        "total_locked": locked_files.len(),
        "total_tasks": distinct_tasks.len(),
        "total_reservations": reservation_result.reservations.len(),
    }))
}

pub(crate) fn release(
    runtime: &OrbitRuntime,
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

pub(crate) fn reserve(
    runtime: &OrbitRuntime,
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
}

/// Whether a task-scope reservation that resolves to zero files is a mistake
/// to refuse, or a legitimate no-op to admit.
///
/// An operator claiming a task's surface before starting work almost
/// certainly wants a real claim: a task with nothing declared should be told
/// so, not handed a reservation ID that holds nothing ([`Self::Refuse`]). The
/// v2 dispatch admission gate reserves the same way to decide whether a task
/// can start, and a task that has not declared any context yet has nothing to
/// serialize against — admitting it trivially is correct there
/// ([`Self::Admit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmptyTaskSurfacePolicy {
    Refuse,
    Admit,
}

/// Reserve a task-lock scope using an index loaded by the calling operation.
///
/// The v2 `reserve_locks` action needs the same task index for its one-poll
/// admission path and its reservation check. Keeping the actual reservation
/// behavior here gives both callers one canonical implementation while letting
/// that action avoid reloading every task bundle.
pub(crate) fn reserve_with_index(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
    reservation_owner: Option<ReservationOwnerContext>,
    index: &TaskLockIndex,
    empty_task_surface_policy: EmptyTaskSurfacePolicy,
) -> Result<Value, OrbitError> {
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
        Ok(json!({
            "reserved": true,
            "reservation_id": reservation_result.reservation_id,
            "expires_at": reservation_result.expires_at,
            "reserved_files": reservation_result.reserved_files,
        }))
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
        Ok(json!({
            "reserved": false,
            "conflicts": conflicts,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TaskLockReservationScope {
    TaskIds(Vec<String>),
    Files(Vec<String>),
}

pub(super) fn parse_task_lock_reservation_scope(
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

pub(crate) fn parse_task_ids(input: &Value) -> Result<Vec<String>, OrbitError> {
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

pub(crate) fn workspace_orbit_dir(runtime: &OrbitRuntime) -> String {
    runtime.paths().orbit_dir.to_string_lossy().into_owned()
}

pub(crate) fn workspace_task_reservation_id(
    runtime: &OrbitRuntime,
) -> Result<Option<String>, OrbitError> {
    match read_workspace_config_optional(&runtime.paths().orbit_dir)? {
        Some(config) => Ok(Some(config.workspace_id)),
        None => Err(OrbitError::Store(format!(
            "task artifact workspace config is missing at '{}'; rebuild the runtime before writing task lock reservations",
            runtime.paths().orbit_dir.join("config.yaml").display()
        ))),
    }
}

/// Return the effective lock surface for one task.
///
/// Every task — leaf, child, or `epic`-tagged root — reserves exactly what it
/// declares. Hierarchy is metadata: a parent never inherits a child's
/// footprint, so conflict admission excludes only the work that genuinely
/// overlaps [ORB-12491].
pub(crate) fn lock_context_files_for_task(task: &Task, workspace_root: &Path) -> Vec<String> {
    declared_context_files(&task.context_files, workspace_root)
        .retained
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Envelope metadata indexed for lock-surface expansion: active tasks,
/// explicitly requested tasks, and their ancestors. One operation builds it
/// once without hydrating task bodies or sidecars; repeated surface expansion
/// then reuses it.
pub(crate) struct TaskLockIndex {
    tasks: BTreeMap<String, TaskEnvelopeV2>,
}

impl TaskLockIndex {
    pub(crate) fn load(
        runtime: &OrbitRuntime,
        requested_task_ids: &[String],
    ) -> Result<Self, OrbitError> {
        let envelopes = runtime
            .task_candidates(&Default::default(), usize::MAX)?
            .items;
        Ok(Self::from_envelopes(envelopes, requested_task_ids))
    }

    fn from_envelopes(envelopes: Vec<TaskEnvelopeV2>, requested_task_ids: &[String]) -> Self {
        let all_tasks = envelopes
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect::<BTreeMap<_, _>>();
        let requested_task_ids = requested_task_ids.iter().collect::<BTreeSet<_>>();
        let seed_ids = all_tasks
            .values()
            .filter(|task| {
                matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)
                    || requested_task_ids.contains(&task.id)
            })
            .map(|task| task.id.clone())
            .collect::<BTreeSet<_>>();
        let mut retained_ids = seed_ids.clone();

        // Parent envelopes are kept so hierarchy stays readable from the index
        // under the same guarded walk the bundle-backed implementation used.
        // They do not widen anyone's lock surface [ORB-12491].
        for task_id in retained_ids.clone() {
            retain_task_ancestors(&task_id, &all_tasks, &mut retained_ids);
        }

        let tasks = all_tasks
            .into_iter()
            .filter(|(task_id, _)| retained_ids.contains(task_id))
            .collect::<BTreeMap<_, _>>();
        Self { tasks }
    }

    pub(crate) fn get(&self, task_id: &str) -> Option<&TaskEnvelopeV2> {
        self.tasks.get(task_id)
    }

    pub(crate) fn tasks(&self) -> impl Iterator<Item = &TaskEnvelopeV2> {
        self.tasks.values()
    }

    fn into_active_lock_surfaces(
        mut self,
        workspace_root: &Path,
    ) -> Vec<(TaskEnvelopeV2, Vec<String>)> {
        let mut active_ids = self
            .tasks
            .values()
            .filter(|task| matches!(task.status, TaskStatus::InProgress | TaskStatus::Review))
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        active_ids.sort_by_key(|task_id| {
            self.tasks.get(task_id).map(|task| {
                (
                    task_lock_status_rank(task.status),
                    task.created_at,
                    task.id.clone(),
                )
            })
        });

        active_ids
            .into_iter()
            .filter_map(|task_id| {
                let files = self
                    .tasks
                    .get(&task_id)
                    .map(|task| self.lock_context_files(task, workspace_root))?;
                self.tasks.remove(&task_id).map(|task| (task, files))
            })
            .collect()
    }

    /// [`lock_context_files_for_task`] over indexed envelopes.
    pub(crate) fn lock_context_files(
        &self,
        task: &TaskEnvelopeV2,
        workspace_root: &Path,
    ) -> Vec<String> {
        self.declared_lock_surface(task, workspace_root).retained
    }

    /// The canonical lock surface for `task` plus the declarations that could
    /// not be canonicalized at all.
    ///
    /// Invalid entries are the only ones a lock surface loses, and they are
    /// reported rather than dropped in silence: a task whose every declaration
    /// is unusable would otherwise read as a claim protecting no files.
    pub(crate) fn declared_lock_surface(
        &self,
        task: &TaskEnvelopeV2,
        workspace_root: &Path,
    ) -> DeclaredContextFiles {
        let mut declared = declared_context_files(&task.context_files, workspace_root);
        declared.retained = declared
            .retained
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        declared.invalid = declared
            .invalid
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        declared
    }

    /// Whether `task_id` has declared any `context_files` entries at all.
    ///
    /// A selector for a file the task has not created yet is a declaration
    /// like any other and reaches [`Self::lock_context_files`] intact, so this
    /// answers the narrower question a task-scope reservation refuses on:
    /// nothing declared at all. A root inherits nothing from its children, so
    /// an empty root declares no surface [ORB-12491].
    pub(crate) fn declares_context_surface(&self, task_id: &str) -> bool {
        self.tasks
            .get(task_id)
            .is_some_and(|task| !task.context_files.is_empty())
    }
}

fn envelope_parent_id(task: &TaskEnvelopeV2) -> Option<&str> {
    task.relations
        .iter()
        .find(|relation| relation.relation_type == TaskRelationType::ChildOf)
        .map(|relation| relation.target.as_str())
}

fn retain_task_ancestors(
    task_id: &str,
    task_lookup: &BTreeMap<String, TaskEnvelopeV2>,
    retained_ids: &mut BTreeSet<String>,
) {
    let mut visited = BTreeSet::from([task_id.to_string()]);
    let mut next_parent_id = task_lookup.get(task_id).and_then(envelope_parent_id);
    for _ in 0..32 {
        let Some(parent_id) = next_parent_id else {
            break;
        };
        if !visited.insert(parent_id.to_string()) {
            break;
        }
        let Some(parent) = task_lookup.get(parent_id) else {
            break;
        };
        retained_ids.insert(parent.id.clone());
        next_parent_id = envelope_parent_id(parent);
    }
}

pub(crate) fn requested_task_files_indexed(
    index: &TaskLockIndex,
    task_ids: &[String],
    workspace_root: &Path,
) -> Result<Vec<String>, OrbitError> {
    let mut requested_files = BTreeSet::new();
    for task_id in task_ids {
        let task = index
            .get(task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.clone()))?;
        requested_files.extend(index.lock_context_files(task, workspace_root));
    }
    Ok(requested_files.into_iter().collect())
}

pub(crate) fn task_lock_conflicts_indexed(
    index: &TaskLockIndex,
    bundle_task_ids: &[String],
    requested_files: &[String],
    workspace_root: &Path,
) -> Vec<TaskLockConflict> {
    let bundle_ids = bundle_task_ids.iter().cloned().collect::<BTreeSet<_>>();
    let requested_files = requested_files.iter().cloned().collect::<BTreeSet<_>>();
    if requested_files.is_empty() {
        return Vec::new();
    }

    let mut tasks: Vec<&TaskEnvelopeV2> = index
        .tasks()
        .filter(|task| {
            matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)
                && !bundle_ids.contains(&task.id)
        })
        .collect();
    tasks.sort_by_key(|task| {
        (
            task_lock_status_rank(task.status),
            task.created_at,
            task.id.clone(),
        )
    });

    let mut conflicts = Vec::new();
    for task in tasks {
        let held_files = index.lock_context_files(task, workspace_root);
        for requested_file in &requested_files {
            if held_files
                .iter()
                .any(|held_file| workspace_relative_paths_overlap(requested_file, held_file))
            {
                conflicts.push(TaskLockConflict {
                    file: requested_file.clone(),
                    held_by: TaskLockHolder::Task,
                    held_by_id: task.id.clone(),
                });
            }
        }
    }

    conflicts.sort_by(|left, right| {
        left.file
            .cmp(&right.file)
            .then(left.held_by_id.cmp(&right.held_by_id))
    });
    conflicts
}

pub(crate) fn merge_task_lock_conflicts(
    left: Vec<TaskLockConflict>,
    right: Vec<TaskLockConflict>,
) -> Vec<TaskLockConflict> {
    let mut merged = left;
    merged.extend(right);
    merged.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| match (a.held_by, b.held_by) {
                (TaskLockHolder::Task, TaskLockHolder::Reservation) => std::cmp::Ordering::Less,
                (TaskLockHolder::Reservation, TaskLockHolder::Task) => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            })
            .then(a.held_by_id.cmp(&b.held_by_id))
    });
    merged.dedup_by(|a, b| {
        a.file == b.file && a.held_by == b.held_by && a.held_by_id == b.held_by_id
    });
    merged
}

pub(crate) fn emit_expired_reservation_events(
    runtime: &OrbitRuntime,
    expired_reservations: &[ExpiredTaskReservation],
) -> Result<(), OrbitError> {
    for expired in expired_reservations {
        record_task_lock_audit_event(
            runtime,
            "task.locks.reserve.expired",
            "orbit.task.locks.reserve",
            Some(expired.reservation_id.as_str()),
            None,
            AuditEventStatus::Success,
            json!({
                "reservation_id": expired.reservation_id,
                "expired_at": expired.expired_at,
            }),
        )?;
    }
    Ok(())
}

pub(crate) fn emit_task_lock_release_event(
    runtime: &OrbitRuntime,
    reservation: &ReleasedTaskReservation,
    release_reason: TaskReservationReleaseReason,
) -> Result<(), OrbitError> {
    record_task_lock_audit_event(
        runtime,
        "task.locks.reserve.released",
        "orbit.task.locks.release",
        Some(reservation.reservation_id.as_str()),
        first_task_id(&reservation.task_ids),
        AuditEventStatus::Success,
        json!({
            "reservation_id": reservation.reservation_id,
            "owner_run_id": reservation.owner_run_id,
            "release_reason": release_reason.as_str(),
            "released_at": reservation.released_at,
        }),
    )
}

fn reservation_actor_label(
    runtime: &OrbitRuntime,
    agent: Option<&str>,
    model: Option<&str>,
) -> Result<String, OrbitError> {
    runtime.actor().resolve_write_label(agent, model)
}

fn record_task_lock_audit_event(
    runtime: &OrbitRuntime,
    command: &str,
    tool_name: &str,
    target_id: Option<&str>,
    task_id: Option<&str>,
    status: AuditEventStatus,
    payload: Value,
) -> Result<(), OrbitError> {
    record_coordination_audit_event(
        runtime,
        CoordinationAuditEvent {
            command,
            tool_name,
            target_type: "task_reservation",
            target_id,
            task_id,
            status,
            payload,
        },
    )
}

/// A task-scope reservation whose bundle declares no `context_files` at all
/// would otherwise mint a real reservation ID that holds nothing — a silent
/// "0 file(s)" success that looks like a claim was taken when it was not.
/// Refuse it by name instead so the caller declares context or falls back to
/// explicit `--file` selectors.
/// Every declared selector on the requested bundle, as stored, that cannot be
/// canonicalized against the workspace root.
fn invalid_declared_selectors(
    index: &TaskLockIndex,
    task_ids: &[String],
    workspace_root: &Path,
) -> Vec<String> {
    let mut invalid = BTreeSet::new();
    for task_id in task_ids {
        if let Some(task) = index.get(task_id) {
            invalid.extend(index.declared_lock_surface(task, workspace_root).invalid);
        }
    }
    invalid.into_iter().collect()
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

fn first_task_id(task_ids: &[String]) -> Option<&str> {
    task_ids.first().map(String::as_str)
}

fn task_lock_to_json(task: &TaskEnvelopeV2, context_files: Vec<String>) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "status": task.status.to_string(),
        "job_run_id": task.job_run_id,
        "crew": task.crew,
        "orchestrator": task.orchestrator,
        "context_files": context_files,
    })
}

fn task_lock_status_rank(status: TaskStatus) -> u8 {
    match status {
        TaskStatus::InProgress => 0,
        TaskStatus::Review => 1,
        _ => 2,
    }
}
