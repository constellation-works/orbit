//! SQLite task reservations and workspace claims.
//!
//! `reservations` holds the `Store` reservation API; `rows` the SQL helpers
//! and row mapping it shares with `workspace_claim` and `leases`.

mod holder_list;
mod lease_queue;
mod leases;
pub(crate) mod notices;
mod reservations;
mod rows;
#[cfg(test)]
mod tests;
mod workspace_claim;

pub(crate) use rows::reserve_files_in_tx;
use rows::{expire_reservations_in_scope, reservation_scope_clause, unique_row_id};

use orbit_common::OrbitError;

use crate::boundary::TaskCommitBoundary;
use crate::contract::{
    LeaseExpandParams, LeaseExpandResult, LeaseInsertParams, LeaseInsertResult, LeaseListResult,
    LeaseMode, LeaseQueueGrantParams, LeaseQueueGrantResult, LeaseRecord, LeaseReleaseParams,
    LeaseReleaseResult, LeaseRenewParams, LeaseRenewResult, TaskReservationCheckParams,
    TaskReservationCheckResult, TaskReservationListResult, TaskReservationOwnedConflictsParams,
    TaskReservationOwnedConflictsResult, TaskReservationReleaseByOwnerParams,
    TaskReservationReleaseByOwnerResult, TaskReservationReleaseParams,
    TaskReservationReleaseResult, TaskReservationReserveParams, TaskReservationReserveResult,
    WorkspaceClaimAcquireParams, WorkspaceClaimAcquireResult, WorkspaceClaimCheckParams,
    WorkspaceClaimCheckResult, WorkspaceClaimReleaseParams, WorkspaceClaimReleaseResult,
    WorkspaceClaimStatusResult,
};
use crate::{ActiveTaskReservation, Store};

/// Reservation and lease records that share the task commit boundary.
#[derive(Clone)]
pub struct TaskReservationRepository {
    pub(crate) store: Store,
    /// The partition's mandatory commit boundary. Reservation
    /// mutations then share the serialization an admission decision holds, so
    /// a reservation cannot be taken between an admission's readiness read and
    /// its commit (ORB-12528).
    pub(crate) coordination: std::sync::Arc<TaskCommitBoundary>,
}

impl TaskReservationRepository {
    #[track_caller]
    fn in_boundary<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        self.coordination.enter_ordinary(op)
    }

    /// Read every task reservation, active or released, oldest first; leases
    /// are not task reservations.
    /// Read-only: it neither expires nor releases rows.
    pub fn list_task_reservation_history(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::contract::TaskReservationRecord>, OrbitError> {
        self.store
            .list_task_reservation_history(workspace_orbit_dir, workspace_id)
    }

    /// Read active path holders, task reservations and hard leases, without
    /// expiring or otherwise mutating rows. Advisory leases hold nothing.
    pub fn inspect_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<ActiveTaskReservation>, OrbitError> {
        self.store
            .inspect_active_task_reservations(workspace_orbit_dir, workspace_id)
    }

    /// Read one active reservation by id with the visibility of
    /// [`Self::inspect_active_task_reservations`]; `None` when it is gone,
    /// released, expired, or outside the workspace.
    pub fn inspect_active_task_reservation(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
        reservation_id: &str,
    ) -> Result<Option<ActiveTaskReservation>, OrbitError> {
        self.store.inspect_active_task_reservation(
            workspace_orbit_dir,
            workspace_id,
            reservation_id,
        )
    }

    pub fn list_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<TaskReservationListResult, OrbitError> {
        // Listing marks expired rows released, so it is a mutation.
        self.in_boundary(|| {
            self.store
                .list_active_task_reservations(workspace_orbit_dir, workspace_id)
        })
    }

    pub fn check_task_reservation_conflicts(
        &self,
        params: TaskReservationCheckParams,
    ) -> Result<TaskReservationCheckResult, OrbitError> {
        self.in_boundary(|| self.store.check_task_reservation_conflicts(&params))
    }

    pub fn reserve_task_reservation(
        &self,
        params: TaskReservationReserveParams,
    ) -> Result<TaskReservationReserveResult, OrbitError> {
        self.in_boundary(|| {
            let conflicts = self
                .coordination
                .frozen_claim_conflicts(&params.requested_files)?;
            if !conflicts.is_empty() {
                return Ok(TaskReservationReserveResult {
                    reserved: false,
                    reservation_id: None,
                    expires_at: None,
                    reserved_files: Vec::new(),
                    conflicts,
                    expired_reservations: Vec::new(),
                });
            }
            self.store.reserve_task_reservation(&params)
        })
    }

    pub fn release_task_reservation(
        &self,
        params: TaskReservationReleaseParams,
    ) -> Result<TaskReservationReleaseResult, OrbitError> {
        self.in_boundary(|| self.store.release_task_reservation(&params))
    }

    pub fn release_task_reservations_by_owner_run_id(
        &self,
        params: TaskReservationReleaseByOwnerParams,
    ) -> Result<TaskReservationReleaseByOwnerResult, OrbitError> {
        self.in_boundary(|| {
            self.store
                .release_task_reservations_by_owner_run_id(&params)
        })
    }

    pub fn list_owned_task_reservation_conflicts(
        &self,
        params: TaskReservationOwnedConflictsParams,
    ) -> Result<TaskReservationOwnedConflictsResult, OrbitError> {
        self.in_boundary(|| self.store.list_owned_task_reservation_conflicts(&params))
    }

    /// Record a path lease, granted or queued, or report the holders that
    /// refuse a hard one asked not to wait.
    pub fn insert_lease(
        &self,
        mut params: LeaseInsertParams,
    ) -> Result<LeaseInsertResult, OrbitError> {
        // A hard lease must not land between an admission's readiness read
        // and its commit, and must not overlap a frozen claim footprint: a
        // claimed task holds its paths like a running one, so the lease
        // waits for it.
        self.in_boundary(|| {
            if params.mode == LeaseMode::Hard {
                params.task_conflicts.extend(
                    self.coordination
                        .frozen_claim_conflicts(&params.selectors)?,
                );
            }
            self.store.insert_lease(&params)
        })
    }

    /// Add selectors to a granted lease; blocked ones on a hard lease queue.
    pub fn expand_lease(
        &self,
        mut params: LeaseExpandParams,
    ) -> Result<LeaseExpandResult, OrbitError> {
        self.in_boundary(|| {
            params.task_conflicts.extend(
                self.coordination
                    .frozen_claim_conflicts(&params.selectors)?,
            );
            self.store.expand_lease(&params)
        })
    }

    /// Grant waiting requests and expansions, in queue order, whose paths
    /// are free.
    pub fn grant_queued_leases(
        &self,
        mut params: LeaseQueueGrantParams,
    ) -> Result<LeaseQueueGrantResult, OrbitError> {
        self.in_boundary(|| {
            let waiting = self.store.inspect_active_leases(
                &params.workspace_orbit_dir,
                params.workspace_id.as_deref(),
            )?;
            for lease in waiting {
                if let Some(conflicts) = params.task_conflicts.get_mut(&lease.lease_id) {
                    conflicts.extend(
                        self.coordination
                            .frozen_claim_conflicts(&lease.queued_selectors)?,
                    );
                }
            }
            self.store.grant_queued_leases(&params)
        })
    }

    /// Move an active lease's `expires_at` forward.
    pub fn renew_lease(&self, params: LeaseRenewParams) -> Result<LeaseRenewResult, OrbitError> {
        self.in_boundary(|| self.store.renew_lease(&params))
    }

    /// End an active lease.
    pub fn release_lease(
        &self,
        params: LeaseReleaseParams,
    ) -> Result<LeaseReleaseResult, OrbitError> {
        self.in_boundary(|| self.store.release_lease(&params))
    }

    /// Read one lease in any state, without mutating rows.
    pub fn get_lease(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
        lease_id: &str,
    ) -> Result<Option<LeaseRecord>, OrbitError> {
        self.store
            .get_lease(workspace_orbit_dir, workspace_id, lease_id)
    }

    /// The workspace's active leases, advisory and hard, without expiring
    /// or otherwise mutating rows.
    pub fn inspect_active_leases(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<LeaseRecord>, OrbitError> {
        self.store
            .inspect_active_leases(workspace_orbit_dir, workspace_id)
    }

    /// The workspace's active leases after lazy expiry.
    pub fn list_active_leases(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<LeaseListResult, OrbitError> {
        // Listing marks expired leases released, so it is a mutation.
        self.in_boundary(|| {
            self.store
                .list_active_leases(workspace_orbit_dir, workspace_id)
        })
    }

    /// Expire elapsed leases and publish one 80%-TTL warning per heartbeat.
    /// Returns the remaining active leases for the owner's landing checks.
    pub fn sweep_lease_lifetimes(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<LeaseListResult, OrbitError> {
        self.in_boundary(|| {
            self.store
                .sweep_lease_lifetimes(workspace_orbit_dir, workspace_id)
        })
    }

    /// Take the exclusive workspace claim [ADR-0352, ORB-10709], or report the
    /// incumbent that refused it.
    pub fn acquire_workspace_claim(
        &self,
        params: WorkspaceClaimAcquireParams,
    ) -> Result<WorkspaceClaimAcquireResult, OrbitError> {
        self.in_boundary(|| self.store.acquire_workspace_claim(&params))
    }

    /// Release the claim with its token, or force-release it.
    pub fn release_workspace_claim(
        &self,
        params: WorkspaceClaimReleaseParams,
    ) -> Result<WorkspaceClaimReleaseResult, OrbitError> {
        self.in_boundary(|| self.store.release_workspace_claim(&params))
    }

    /// The active claim after lazy expiry, or `None` when unclaimed.
    pub fn show_workspace_claim(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<WorkspaceClaimStatusResult, OrbitError> {
        // Showing marks expired claims released, so it is a mutation.
        self.in_boundary(|| {
            self.store
                .show_workspace_claim(workspace_orbit_dir, workspace_id)
        })
    }

    /// Whether a presented token satisfies the active claim. The comparison
    /// stays inside the store so a refusal never has to carry the incumbent's
    /// token back out.
    pub fn check_workspace_claim(
        &self,
        params: WorkspaceClaimCheckParams,
    ) -> Result<WorkspaceClaimCheckResult, OrbitError> {
        self.in_boundary(|| self.store.check_workspace_claim(&params))
    }
}
