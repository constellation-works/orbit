//! The holders that overlap a leased or checked path: task context locks,
//! task reservations and other leases.

use std::ops::ControlFlow;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::fs::selector::{OverlapScope, overlaps};
use orbit_store::contracts::{LeaseMode, LeaseRecord, TaskReservationKind};
use serde::Serialize;

use super::super::commands::{workspace_orbit_dir, workspace_task_reservation_id};
use super::super::index::{TaskLockIndex, holds_context_lock, task_lock_status_rank};
use super::response::ResponseBudget;
use crate::RuntimeKernel;

/// One holder overlapping a checked or leased path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct LeaseHolder {
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
pub(super) struct PathHolders<'a> {
    index: &'a TaskLockIndex,
    reservations: Vec<(String, Vec<String>)>,
    leases: Vec<LeaseRecord>,
}

impl<'a> PathHolders<'a> {
    pub(super) fn load(
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
    pub(super) fn overlapping(
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
pub(super) fn selector_covers(selector: &str, path: &str) -> bool {
    let (Some(held), Some(checked)) = (OverlapScope::parse(selector), OverlapScope::parse(path))
    else {
        return false;
    };
    let OverlapScope::Anchored { path: anchor, .. } = &checked else {
        return false;
    };
    held.overlap_point(&checked) == Some(anchor.as_str())
}
