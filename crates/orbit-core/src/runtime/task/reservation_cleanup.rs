//! Detection and cleanup of stale task reservations.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    ActiveTaskReservation, ReleasedTaskReservation, TaskReservationOwnedConflictsParams,
    TaskReservationReleaseByOwnerParams, TaskReservationReleaseParams,
    TaskReservationReleaseReason,
};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRunState;
use serde_json::json;

use crate::OrbitRuntime;

use super::locks::{
    emit_expired_reservation_events, emit_task_lock_release_event, workspace_orbit_dir,
    workspace_task_reservation_id,
};

/// Workspace-wide facts consulted while classifying stale reservations.
struct StaleReservationContext {
    /// Run ids the orphan classifiers already consider conclusively orphaned.
    orphaned_run_ids: BTreeSet<String>,
    task_statuses: BTreeMap<String, TaskStatus>,
}

/// A task reservation that Orbit can prove is no longer protecting live work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaleTaskReservation {
    pub reservation_id: String,
    pub task_ids: Vec<String>,
    pub owner_run_id: Option<String>,
    pub reason: String,
}

impl OrbitRuntime {
    /// Read-only classification used by `orbit doctor`.
    ///
    /// A reservation is conclusive only when its owner run is absent,
    /// terminal, or already satisfies the existing orphan-run classifier; or
    /// when it is unowned and every associated task is `done` (Orbit's only
    /// terminal task status). Empty/missing/mixed task associations remain
    /// ambiguous and are deliberately ignored.
    pub fn list_stale_task_reservations(&self) -> Result<Vec<StaleTaskReservation>, OrbitError> {
        let reservations = self.inspect_active_reservations()?;
        let context = self.stale_reservation_context()?;
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

    /// Release reservations that are still conclusively stale when re-read
    /// immediately before their individual mutation. Replays are no-ops.
    pub fn release_stale_task_reservations(&self) -> Result<usize, OrbitError> {
        let candidate_ids = self
            .list_stale_task_reservations()?
            .into_iter()
            .map(|candidate| candidate.reservation_id)
            .collect::<Vec<_>>();
        if candidate_ids.is_empty() {
            return Ok(0);
        }
        // The orphan-run and task-status facts are read once for the pass;
        // only the reservation row itself is re-read before each release,
        // since that is the row another operator could have released or
        // re-owned in the meantime.
        let context = self.stale_reservation_context()?;
        let mut released = 0;
        for reservation_id in candidate_ids {
            let current = self
                .inspect_active_reservations()?
                .into_iter()
                .find(|reservation| reservation.reservation_id == reservation_id);
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

    fn inspect_active_reservations(&self) -> Result<Vec<ActiveTaskReservation>, OrbitError> {
        self.stores()
            .task_reservations()
            .inspect_active_task_reservations(
                &workspace_orbit_dir(self),
                workspace_task_reservation_id(self)?.as_deref(),
            )
    }

    /// The workspace-wide facts stale classification consults, read once per
    /// pass instead of once per reservation: the orphan classifier scans
    /// every job run and the task sweep reads every task.
    fn stale_reservation_context(&self) -> Result<StaleReservationContext, OrbitError> {
        let mut orphaned_run_ids = self
            .list_orphaned_running_job_runs()?
            .into_iter()
            .map(|orphan| orphan.run_id)
            .collect::<BTreeSet<_>>();
        orphaned_run_ids.extend(
            self.list_orphaned_pending_job_runs()?
                .into_iter()
                .map(|orphan| orphan.run_id),
        );
        let task_statuses = self
            .list_tasks()?
            .into_iter()
            .map(|task| (task.id, task.status))
            .collect::<BTreeMap<_, _>>();
        Ok(StaleReservationContext {
            orphaned_run_ids,
            task_statuses,
        })
    }

    fn classify_stale_task_reservation(
        &self,
        reservation: &ActiveTaskReservation,
        context: &StaleReservationContext,
    ) -> Result<Option<String>, OrbitError> {
        if let Some(owner_run_id) = reservation.owner_run_id.as_deref() {
            let Some(run) = self.get_job_run_backend(owner_run_id)? else {
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

    pub(crate) fn finalize_job_run_with_reservation_cleanup(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
        release_reason: TaskReservationReleaseReason,
    ) -> Result<bool, OrbitError> {
        // Capture the state the run was in *before* finalizing:
        // `finalize_run` reports `changed == true` even when re-finalizing an
        // already-terminal run, so it can't distinguish the terminalizing write
        // from a replay. The coupling-out block must fire only on the actual
        // transition into a terminal failure state.
        let prior_state = self.get_job_run_backend(run_id)?.map(|run| run.state);
        let was_terminal_before = prior_state.is_some_and(JobRunState::is_terminal);
        let changed =
            self.stores()
                .jobs()
                .finalize_job_run(run_id, state, finished_at, duration_ms)?;
        // [ORB-10597] The store keeps the first terminal state and drops this
        // one. An identical re-finalization is an ordinary idempotent replay; a
        // *different* terminal outcome is a real contradiction — most sharply,
        // a run condemned to `interrupted` while it was still working and then
        // reporting the success it actually reached. That must not vanish, so
        // it is recorded on the run instead of discarded. See
        // `application::job::run::conflict` for why the recorded state still wins.
        // A cancellation request that loses to a worker's terminal outcome is
        // reported to the caller as `already_terminal`; it did not produce a
        // second durable outcome. The inverse remains a real contradiction: a
        // worker reporting success/failure after `cancelled` is still recorded.
        if let Some(prior) = prior_state.filter(|prior| {
            prior.is_terminal() && *prior != state && state != JobRunState::Cancelled
        }) {
            self.record_terminal_outcome_conflict(run_id, prior, state, finished_at);
        }
        if state.is_terminal() {
            self.best_effort_release_task_reservations_for_owner_run_id(run_id, release_reason);
            // Coupling-out: block the run's coupled tasks only on the first
            // terminalization into a failure state. Gating on
            // `!was_terminal_before` keeps this idempotent — a replayed
            // terminalization is a no-op and never clobbers a task a human
            // already moved on. Blocking is best-effort so a status-write
            // failure never blocks the run from terminalizing or its
            // reservations/file locks from being released.
            if !was_terminal_before && super::block_on_run_failure::is_workflow_failure_state(state)
            {
                self.best_effort_block_tasks_for_failed_run(run_id, state);
            }
        }
        Ok(changed)
    }

    pub(crate) fn release_task_reservations_for_owner_run_id(
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

    pub(crate) fn best_effort_release_task_reservations_for_owner_run_id(
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

    pub(crate) fn reconcile_stale_owned_reservations_for_files(
        &self,
        requested_files: &[String],
        limit: usize,
    ) -> Result<Vec<ReleasedTaskReservation>, OrbitError> {
        let candidates = self
            .stores()
            .task_reservations()
            .list_owned_task_reservation_conflicts(TaskReservationOwnedConflictsParams {
                workspace_orbit_dir: workspace_orbit_dir(self),
                workspace_id: workspace_task_reservation_id(self)?,
                requested_files: requested_files.to_vec(),
                limit,
            })?;
        emit_expired_reservation_events(self, &candidates.expired_reservations)?;

        let mut released = Vec::new();
        let mut inspected_owner_run_ids = BTreeSet::new();
        for reservation in candidates.reservations {
            let Some(owner_run_id) = reservation.owner_run_id.as_deref() else {
                continue;
            };
            if !inspected_owner_run_ids.insert(owner_run_id.to_string()) {
                continue;
            }

            match self.get_job_run_backend(owner_run_id)? {
                Some(run) if run.state.is_terminal() => {
                    released.extend(self.release_task_reservations_for_owner_run_id(
                        owner_run_id,
                        TaskReservationReleaseReason::StaleRunReconciled,
                    )?);
                }
                Some(run) => {
                    if self.reconcile_stale_job_run(&run)? {
                        released.extend(self.release_task_reservations_for_owner_run_id(
                            owner_run_id,
                            TaskReservationReleaseReason::StaleRunReconciled,
                        )?);
                    }
                }
                None => {
                    released.extend(self.release_task_reservations_for_owner_run_id(
                        owner_run_id,
                        TaskReservationReleaseReason::StaleRunReconciled,
                    )?);
                }
            }
        }
        Ok(released)
    }
}
