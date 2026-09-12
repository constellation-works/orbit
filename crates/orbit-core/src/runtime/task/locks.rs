//! Task reservation and file-lock operations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::fs::path::workspace_relative_paths_overlap;
use orbit_common::fs::selector::{Selector, canonical_selector_in_workspace};
use orbit_common::fs::task_io::prune_missing_context_files;
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
use orbit_types::identity::normalize_optional_attribution_label;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::runtime::coordination_audit::{CoordinationAuditEvent, record_coordination_audit_event};
use crate::runtime::task::canonicalize_context_files_for_read;

pub(crate) fn list(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let reservation_result = runtime
        .stores()
        .task_reservations()
        .list_active_task_reservations(&workspace_orbit_dir(runtime), workspace_id.as_deref())?;
    emit_expired_reservation_events(runtime, &reservation_result.expired_reservations)?;

    let index = TaskLockIndex::from_tasks(runtime.list_tasks()?);
    let mut tasks: Vec<&Task> = index
        .tasks()
        .filter(|task| matches!(task.status, TaskStatus::InProgress | TaskStatus::Review))
        .collect();
    tasks.sort_by_key(|task| {
        (
            task_lock_status_rank(task.status),
            task.created_at,
            task.id.clone(),
        )
    });

    // Expand each task's lock surface once and reuse it for both projections
    // below. The expansion prunes every declared selector against the
    // filesystem and, for an epic root, unions the surface of every
    // descendant — so computing it per projection doubled the syscalls and the
    // descendant walk for a listing that has a single answer.
    let repo_root = runtime.paths().repo_root.as_path();
    let locked_surfaces: Vec<(Task, Vec<String>)> = tasks
        .into_iter()
        .map(|task| {
            let files = index.lock_context_files(task, repo_root);
            (task.clone(), files)
        })
        .collect();

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
                    ),
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
                ),
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
    let index = TaskLockIndex::load(runtime)?;
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
    if !(1..=7200).contains(&ttl_seconds) {
        return Err(OrbitError::InvalidInput(
            "`ttl_seconds` must be between 1 and 7200 seconds".to_string(),
        ));
    }

    let actor = reservation_actor_label(runtime, agent.as_deref(), model.as_deref());
    let workspace_id = workspace_task_reservation_id(runtime)?;
    let repo_root = runtime.paths().repo_root.as_path();
    let (task_ids, requested_files) = match &reservation_scope {
        TaskLockReservationScope::TaskIds(task_ids) => {
            // Validate every id exists before judging whether the bundle
            // declares a surface, so an unknown task id is still reported as
            // not-found rather than folded into this refusal.
            let requested_files = requested_task_files_indexed(index, task_ids, repo_root)?;
            if empty_task_surface_policy == EmptyTaskSurfacePolicy::Refuse
                && task_ids
                    .iter()
                    .all(|task_id| !index.declares_context_surface(task_id))
            {
                return Err(no_lock_surface_error(task_ids));
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
/// An active epic root owns the union of every descendant's declared files so
/// conflict admission can keep unrelated work moving while excluding only
/// leaves that overlap the epic's actual family.
pub(crate) fn lock_context_files_for_task(
    task: &Task,
    task_lookup: &BTreeMap<String, Task>,
    workspace_root: &Path,
) -> Vec<String> {
    let mut files = existing_context_files_at_root(task, workspace_root)
        .into_iter()
        .collect::<BTreeSet<_>>();
    if task.tags.iter().any(|tag| tag == "epic") {
        for candidate in task_lookup.values() {
            if task_is_descendant_of(candidate, &task.id, task_lookup) {
                files.extend(existing_context_files_at_root(candidate, workspace_root));
            }
        }
    }
    files.into_iter().collect()
}

fn existing_context_files_at_root(task: &Task, workspace_root: &Path) -> Vec<String> {
    let canonical = canonicalize_context_files_for_read(&task.context_files, workspace_root);
    let (kept, _dropped) = prune_missing_context_files(workspace_root, canonical);
    kept
}

fn task_is_descendant_of(
    task: &Task,
    ancestor_id: &str,
    task_lookup: &BTreeMap<String, Task>,
) -> bool {
    let mut visited = BTreeSet::from([task.id.clone()]);
    let mut next_parent_id = task.parent_id();
    for _ in 0..32 {
        let Some(parent_id) = next_parent_id else {
            return false;
        };
        if parent_id == ancestor_id {
            return true;
        }
        if !visited.insert(parent_id.to_string()) {
            return false;
        }
        let Some(parent) = task_lookup.get(parent_id) else {
            return false;
        };
        next_parent_id = parent.parent_id();
    }
    false
}

/// The task store indexed for lock-surface expansion: the id lookup plus,
/// for each epic root, every task below it. One operation builds it once; a
/// reserve that inspects forty active tasks then expands forty surfaces
/// without re-reading the store or re-walking every task's parent chain for
/// each epic it meets.
pub(crate) struct TaskLockIndex {
    tasks: BTreeMap<String, Task>,
    epic_descendants: BTreeMap<String, Vec<String>>,
}

impl TaskLockIndex {
    pub(crate) fn load(runtime: &OrbitRuntime) -> Result<Self, OrbitError> {
        Ok(Self::from_tasks(runtime.stores().tasks().list_tasks()?))
    }

    pub(crate) fn from_tasks(tasks: Vec<Task>) -> Self {
        let tasks = tasks
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect::<BTreeMap<_, _>>();
        let mut epic_descendants: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for task in tasks.values() {
            for epic_id in epic_ancestor_ids(task, &tasks) {
                epic_descendants
                    .entry(epic_id)
                    .or_default()
                    .push(task.id.clone());
            }
        }
        Self {
            tasks,
            epic_descendants,
        }
    }

    pub(crate) fn get(&self, task_id: &str) -> Option<&Task> {
        self.tasks.get(task_id)
    }

    pub(crate) fn tasks(&self) -> impl Iterator<Item = &Task> {
        self.tasks.values()
    }

    /// [`lock_context_files_for_task`] over the precomputed epic families.
    pub(crate) fn lock_context_files(&self, task: &Task, workspace_root: &Path) -> Vec<String> {
        let mut files = existing_context_files_at_root(task, workspace_root)
            .into_iter()
            .collect::<BTreeSet<_>>();
        if task.tags.iter().any(|tag| tag == "epic") {
            for descendant in self
                .epic_descendants
                .get(&task.id)
                .into_iter()
                .flatten()
                .filter_map(|id| self.tasks.get(id))
            {
                files.extend(existing_context_files_at_root(descendant, workspace_root));
            }
        }
        files.into_iter().collect()
    }

    /// Whether `task_id` (or, for an epic root, any descendant) has declared
    /// any `context_files` entries at all, independent of whether those
    /// selectors currently resolve to an existing path.
    ///
    /// This is deliberately not [`Self::lock_context_files`]: a task can
    /// declare a real selector for a file it hasn't created yet, and that is
    /// an existing, unrelated no-op the domain already tolerates (the
    /// selector is simply pruned when computing the lock surface). Only the
    /// narrower case — nothing declared at all — is what a task-scope
    /// reservation should refuse.
    pub(crate) fn declares_context_surface(&self, task_id: &str) -> bool {
        let Some(task) = self.tasks.get(task_id) else {
            return false;
        };
        if !task.context_files.is_empty() {
            return true;
        }
        if task.tags.iter().any(|tag| tag == "epic") {
            return self
                .epic_descendants
                .get(&task.id)
                .into_iter()
                .flatten()
                .filter_map(|id| self.tasks.get(id))
                .any(|descendant| !descendant.context_files.is_empty());
        }
        false
    }
}

/// Every epic-tagged ancestor on `task`'s parent chain, under the same hop
/// and cycle guards as [`task_is_descendant_of`].
fn epic_ancestor_ids(task: &Task, task_lookup: &BTreeMap<String, Task>) -> Vec<String> {
    let mut epics = Vec::new();
    let mut visited = BTreeSet::from([task.id.clone()]);
    let mut next_parent_id = task.parent_id();
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
        if parent.tags.iter().any(|tag| tag == "epic") {
            epics.push(parent.id.clone());
        }
        next_parent_id = parent.parent_id();
    }
    epics
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

    let mut tasks: Vec<&Task> = index
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
) -> String {
    normalize_optional_attribution_label(model.or(agent), model)
        .unwrap_or_else(|| runtime.actor_label().to_string())
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

fn task_lock_to_json(task: &Task, context_files: Vec<String>) -> Value {
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
