//! Detection and cleanup of stale task reservations.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use orbit_common::OrbitError;
use orbit_store::contract::{
    ActiveTaskReservation, ReleasedTaskReservation, TaskReservationReleaseByOwnerParams,
    TaskReservationReleaseParams, TaskReservationReleaseReason,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRun;
use serde_json::json;

use crate::RuntimeKernel;

use crate::stored::locks::{
    emit_expired_reservation_events, emit_task_lock_release_event, workspace_orbit_dir,
    workspace_task_reservation_id,
};

/// Workspace-wide facts consulted while classifying stale reservations.
struct StaleReservationContext {
    /// Run ids the orphan classifiers already consider conclusively orphaned.
    orphaned_run_ids: BTreeSet<String>,
    task_statuses: BTreeMap<String, TaskStatus>,
    /// Owner-run lookups made during this pass, keyed by run id, so several
    /// reservations sharing an owner cost one store read. Stale answers only
    /// err toward keeping a reservation: a terminal state is final, and a run
    /// cached as live is at worst released by the next repair pass.
    run_cache: RefCell<HashMap<String, Option<JobRun>>>,
}

/// A task reservation that Orbit can prove is no longer protecting live work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTaskReservation {
    pub reservation_id: String,
    pub task_ids: Vec<String>,
    pub owner_run_id: Option<String>,
    pub reason: String,
}

impl RuntimeKernel {
    /// Read-only classification used by `orbit doctor`.
    ///
    /// A reservation is conclusive only when its owner run is absent,
    /// terminal, or already satisfies the existing orphan-run classifier; or
    /// when it is unowned and every associated task is `done` (Orbit's only
    /// terminal task status). Empty/missing/mixed task associations remain
    /// ambiguous and are deliberately ignored.
    ///
    /// `reservations` are the active reservations ([`Self::inspect_active_reservations`])
    /// and `orphaned_run_ids` the runs the caller's orphan classifier already
    /// considers conclusively orphaned, which it reads after them.
    pub fn list_stale_task_reservations_among(
        &self,
        reservations: Vec<ActiveTaskReservation>,
        orphaned_run_ids: BTreeSet<String>,
    ) -> Result<Vec<StaleTaskReservation>, OrbitError> {
        let context = self.stale_reservation_context(orphaned_run_ids)?;
        let mut stale = Vec::new();
        for reservation in reservations {
            if let Some(reason) = self.classify_stale_task_reservation(&reservation, &context)? {
                stale.push(StaleTaskReservation {
                    reservation_id: reservation.reservation_id,
                    task_ids: reservation.task_ids,
                    owner_run_id: reservation.owner_run_id,
                    reason,
                });
            }
        }
        Ok(stale)
    }

    /// Release the `candidate_ids` reservations that are still conclusively
    /// stale when re-read immediately before their individual mutation.
    /// Replays are no-ops. `orphaned_run_ids` is as in
    /// [`Self::list_stale_task_reservations_among`].
    pub fn release_task_reservations_still_stale(
        &self,
        candidate_ids: Vec<String>,
        orphaned_run_ids: BTreeSet<String>,
    ) -> Result<usize, OrbitError> {
        // The orphan-run and task-status facts are read once for the pass;
        // only the reservation row itself is re-read before each release,
        // since that is the row another operator could have released or
        // re-owned in the meantime.
        let context = self.stale_reservation_context(orphaned_run_ids)?;
        let mut released = 0;
        for reservation_id in candidate_ids {
            let current = self
                .stores()
                .task_reservations()
                .inspect_active_task_reservation(
                    &workspace_orbit_dir(self),
                    workspace_task_reservation_id(self)?.as_deref(),
                    &reservation_id,
                )?;
            let Some(current) = current else {
                continue;
            };
            let Some(stale_reason) = self.classify_stale_task_reservation(&current, &context)?
            else {
                continue;
            };
            let result = self.stores().task_reservations().release_task_reservation(
                TaskReservationReleaseParams {
                    workspace_orbit_dir: workspace_orbit_dir(self),
                    workspace_id: workspace_task_reservation_id(self)?,
                    reservation_id: current.reservation_id.clone(),
                    release_reason: TaskReservationReleaseReason::DoctorStaleTaskLock,
                    release_metadata_json: Some(
                        json!({
                            "source": "orbit doctor --fix-stale-task-locks",
                            "stale_reason": stale_reason,
                            "owner_run_id": current.owner_run_id,
                            "task_ids": current.task_ids,
                        })
                        .to_string(),
                    ),
                },
            )?;
            emit_expired_reservation_events(self, &result.expired_reservations)?;
            if let Some(reservation) = result.reservation.as_ref() {
                emit_task_lock_release_event(
                    self,
                    reservation,
                    TaskReservationReleaseReason::DoctorStaleTaskLock,
                )?;
                released += 1;
            }
        }
        Ok(released)
    }

    /// Every active task reservation of this workspace.
    pub fn inspect_active_reservations(&self) -> Result<Vec<ActiveTaskReservation>, OrbitError> {
        self.stores()
            .task_reservations()
            .inspect_active_task_reservations(
                &workspace_orbit_dir(self),
                workspace_task_reservation_id(self)?.as_deref(),
            )
    }

    /// The workspace-wide facts stale classification consults, read once per
    /// pass instead of once per reservation: the caller's orphan classifier
    /// scans every job run and the task sweep uses the status projection.
    fn stale_reservation_context(
        &self,
        orphaned_run_ids: BTreeSet<String>,
    ) -> Result<StaleReservationContext, OrbitError> {
        let task_statuses = self.stored().task().status_index()?;
        Ok(StaleReservationContext {
            orphaned_run_ids,
            task_statuses,
            run_cache: RefCell::new(HashMap::new()),
        })
    }

    fn cached_owner_run(
        &self,
        run_id: &str,
        context: &StaleReservationContext,
    ) -> Result<Option<JobRun>, OrbitError> {
        if let Some(cached) = context.run_cache.borrow().get(run_id) {
            return Ok(cached.clone());
        }
        let run = self.stores().jobs().get_job_run(run_id)?;
        context
            .run_cache
            .borrow_mut()
            .insert(run_id.to_string(), run.clone());
        Ok(run)
    }

    fn classify_stale_task_reservation(
        &self,
        reservation: &ActiveTaskReservation,
        context: &StaleReservationContext,
    ) -> Result<Option<String>, OrbitError> {
        if let Some(owner_run_id) = reservation.owner_run_id.as_deref() {
            let Some(run) = self.cached_owner_run(owner_run_id, context)? else {
                return Ok(Some(format!("owner run {owner_run_id} is absent")));
            };
            if run.state.is_terminal() {
                return Ok(Some(format!(
                    "owner run {owner_run_id} is terminal ({})",
                    run.state
                )));
            }
            if context.orphaned_run_ids.contains(owner_run_id) {
                return Ok(Some(format!(
                    "owner run {owner_run_id} is conclusively orphaned ({})",
                    run.state
                )));
            }
            return Ok(None);
        }

        if reservation.task_ids.is_empty() {
            return Ok(None);
        }
        let all_done = reservation
            .task_ids
            .iter()
            .all(|task_id| context.task_statuses.get(task_id) == Some(&TaskStatus::Done));
        if all_done {
            Ok(Some(format!(
                "unowned reservation references only terminal done task(s): {}",
                reservation.task_ids.join(", ")
            )))
        } else {
            Ok(None)
        }
    }

    pub fn release_task_reservations_for_owner_run_id(
        &self,
        owner_run_id: &str,
        release_reason: TaskReservationReleaseReason,
    ) -> Result<Vec<ReleasedTaskReservation>, OrbitError> {
        let result = self
            .stores()
            .task_reservations()
            .release_task_reservations_by_owner_run_id(TaskReservationReleaseByOwnerParams {
                workspace_orbit_dir: workspace_orbit_dir(self),
                workspace_id: workspace_task_reservation_id(self)?,
                owner_run_id: owner_run_id.to_string(),
                release_reason,
                release_metadata_json: Some(
                    json!({
                        "owner_run_id": owner_run_id,
                        "release_reason": release_reason.as_str(),
                    })
                    .to_string(),
                ),
            })?;
        emit_expired_reservation_events(self, &result.expired_reservations)?;
        for reservation in &result.released_reservations {
            emit_task_lock_release_event(self, reservation, release_reason)?;
        }
        Ok(result.released_reservations)
    }

    pub fn best_effort_release_task_reservations_for_owner_run_id(
        &self,
        owner_run_id: &str,
        release_reason: TaskReservationReleaseReason,
    ) {
        if let Err(error) =
            self.release_task_reservations_for_owner_run_id(owner_run_id, release_reason)
        {
            tracing::warn!(
                owner_run_id = owner_run_id,
                release_reason = release_reason.as_str(),
                "failed to release task reservations for terminal job run: {error}"
            );
        }
    }
}
