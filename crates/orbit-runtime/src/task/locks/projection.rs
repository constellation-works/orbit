//! Observational lock listing and contention using the admission surfaces.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_common::fs::overlap_index::OverlapIndex;
use orbit_store::contracts::{ActiveTaskReservation, TaskListFilter};
use orbit_types::task::{TaskEnvelopeV2, TaskStatus};
use serde_json::{Value, json};

use super::commands::{workspace_orbit_dir, workspace_task_reservation_id};
use super::index::{canonical_lock_surface, holds_context_lock, task_lock_status_rank};
use crate::RuntimeKernel;
use crate::task::declared_context_files;

impl RuntimeKernel {
    /// Observe enforced task holders and active reservations, plus per-selector
    /// contention from in-progress holders. No tool dispatch, mutation boundary,
    /// expiry, recovery, index repair or audit write occurs. This snapshot is
    /// informational; admission continues to use its coordinated checks.
    pub fn task_locks_snapshot(&self) -> Result<Value, OrbitError> {
        let workspace_id = workspace_task_reservation_id(self)?;
        let reservations = self
            .stores()
            .task_reservations()
            .inspect_active_task_reservations(
                &workspace_orbit_dir(self),
                workspace_id.as_deref(),
            )?;
        // One population serves both holders and requesters. History and
        // reservation-only hierarchy checks are unnecessary for this read.
        let candidates = self.stores().tasks().observe_task_candidates(
            &TaskListFilter {
                statuses: Some(vec![
                    TaskStatus::Backlog,
                    TaskStatus::InProgress,
                    TaskStatus::Review,
                ]),
                ..Default::default()
            },
            usize::MAX,
        )?;
        let root = &self.paths().repo_root;
        let surfaces = candidates
            .items
            .iter()
            .map(|task| {
                let entries = task
                    .context_files
                    .iter()
                    .map(|selector| declared_context_files(std::slice::from_ref(selector), root))
                    .collect::<Vec<_>>();
                let surface = canonical_lock_surface(
                    entries
                        .iter()
                        .flat_map(|entry| entry.retained.iter().cloned()),
                    self.unlocked_context_paths(),
                );
                (task, entries, surface)
            })
            .collect::<Vec<_>>();
        let mut locked_surfaces = surfaces
            .iter()
            .filter(|(task, _, _)| holds_context_lock(task.status, &task.tags))
            .map(|(task, _, surface)| ((*task).clone(), surface.clone()))
            .collect::<Vec<_>>();
        locked_surfaces.sort_by_key(|(task, _)| {
            (
                task_lock_status_rank(task.status),
                task.created_at,
                task.id.clone(),
            )
        });
        let mut contenders = OverlapIndex::new();
        for (task, files) in &locked_surfaces {
            if task.status == TaskStatus::InProgress {
                for file in files {
                    contenders.insert(file, (&task.id, file));
                }
            }
        }
        let contention = surfaces.iter()
            .filter(|(task, _, _)| matches!(task.status, TaskStatus::Backlog | TaskStatus::InProgress))
            .map(|(task, expanded, surface)| {
                let entries = task.context_files.iter().zip(expanded).map(|(selector, expanded)| {
                    // Apply unlocked rules across the whole task, so a mixed
                    // requester does not re-lock its docs one selector at a time.
                    let matches = expanded.retained.iter()
                        .filter(|requested| surface.binary_search(requested).is_ok())
                        .flat_map(|requested| contenders.overlapping(requested))
                        .filter(|(_, (id, _))| **id != task.id)
                        .map(|(_, (id, held))| json!({"id": id, "status": "in-progress", "selector": held}))
                        .collect::<Vec<_>>();
                    json!({"selector": selector, "known": expanded.invalid.is_empty(), "contenders": matches})
                }).collect::<Vec<_>>();
                (task.id.clone(), json!({"status": task.status.to_string(), "entries": entries}))
            }).collect::<BTreeMap<_, _>>();
        let mut locks = lock_listing_json(&locked_surfaces, &reservations);
        locks["context_contention"] = json!(contention);
        Ok(locks)
    }
}

pub(super) fn lock_listing_json(
    locked_surfaces: &[(TaskEnvelopeV2, Vec<String>)],
    reservations: &[ActiveTaskReservation],
) -> Value {
    let locked_files: BTreeSet<String> = locked_surfaces
        .iter()
        .flat_map(|(_, files)| files.iter().cloned())
        .chain(
            reservations
                .iter()
                .flat_map(|reservation| reservation.files.iter().cloned()),
        )
        .collect();
    let by_reservation = reservations
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
            reservations
                .iter()
                .flat_map(|reservation| reservation.task_ids.iter().map(String::as_str)),
        )
        .collect();

    json!({
        "locked_files": locked_files.iter().cloned().collect::<Vec<_>>(),
        "by_task": locked_surfaces
            .iter()
            .map(|(task, files)| task_lock_to_json(task, files.clone()))
            .collect::<Vec<_>>(),
        "by_reservation": by_reservation,
        "total_locked": locked_files.len(),
        "total_tasks": distinct_tasks.len(),
        "total_reservations": reservations.len(),
    })
}

fn task_lock_to_json(task: &TaskEnvelopeV2, context_files: Vec<String>) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "status": task.status.to_string(),
        "job_run_id": task.job_run_id,
        "crew": task.crew,
        "crew_source": task.crew_source,
        "orchestrator": task.orchestrator,
        "context_files": context_files,
    })
}
