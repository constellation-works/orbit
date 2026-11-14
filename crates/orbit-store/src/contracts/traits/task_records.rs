use crate::contracts::TaskRow;
use crate::contracts::params::*;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ArtifactManifestFileV2, TaskArtifact, TaskComment, TaskHistoryEntry, TaskStatus,
};

pub trait TaskDocumentStoreBackend: Send + Sync {
    fn update_task_document(
        &self,
        id: &str,
        params: TaskDocumentUpdateParams,
    ) -> Result<TaskRow, OrbitError>;
    /// Rewrite the task's stored pre-family tool names (`required_tools`,
    /// `granted_tools`) to family actions and record the rewrite in its
    /// history. `Ok(false)` when it holds none; no other field changes.
    fn migrate_task_tool_names(&self, id: &str, actor: &str) -> Result<bool, OrbitError>;
}

pub trait TaskHistoryStoreBackend: Send + Sync {
    fn get_task_comments(&self, id: &str) -> Result<Option<Vec<TaskComment>>, OrbitError>;
    fn get_task_history(&self, id: &str) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError>;
    /// Evidence that a task's history and comments are unchanged, taken
    /// without reading them: equal stamps mean both reads would return what
    /// they returned when the earlier stamp was taken, or something newer
    /// that a later stamp catches. `None` when this backend cannot vouch for
    /// them, so the caller reads instead of reusing anything derived.
    fn task_history_stamp(&self, _id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }
    /// [`Self::task_history_stamp`] for each listed task, in one pass. A
    /// task is absent when this backend cannot vouch for its logs.
    fn task_history_stamps(
        &self,
        _ids: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, OrbitError> {
        Ok(Default::default())
    }
    /// Evidence that each listed task's whole bundle is unchanged, taken
    /// without reading it: equal stamps mean a whole-bundle read would
    /// return, or fail, as it did when the earlier stamp was taken, with the
    /// limits of [`Self::task_history_stamp`]. A task is absent when this
    /// backend cannot vouch for its bundle.
    fn task_bundle_stamps(
        &self,
        _ids: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, OrbitError> {
        Ok(Default::default())
    }
    fn update_task_history(
        &self,
        id: &str,
        params: TaskHistoryUpdateParams,
    ) -> Result<TaskRow, OrbitError>;
    /// Refuse a work-starting history write whose effective footprint
    /// overlaps a protecting execution claim, before a coordinated caller
    /// persists any companion document or artifact edits. The guard inside
    /// `update_task_history` remains authoritative; this pre-flight only
    /// keeps a deterministic refusal from leaving companion writes behind.
    /// `context_files` is the replacement footprint when the same update
    /// changes it, otherwise the stored footprint applies.
    fn check_ordinary_work_start_footprint(
        &self,
        _id: &str,
        _status: Option<TaskStatus>,
        _status_event: Option<&str>,
        _context_files: Option<&[String]>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }
}

pub trait TaskArtifactStoreBackend: Send + Sync {
    /// Read one immutable payload named by a manifest the caller already
    /// loaded from this task. Verify its bytes without reopening the bundle.
    /// Backends without a snapshot reader retain their ordinary artifact read.
    fn task_artifact_from_manifest(
        &self,
        id: &str,
        file: &ArtifactManifestFileV2,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        self.get_task_artifact(id, &file.path)
    }

    fn get_task_artifact_manifest(
        &self,
        _id: &str,
    ) -> Result<Option<Vec<ArtifactManifestFileV2>>, OrbitError> {
        Err(OrbitError::Store(
            "task artifact manifest read is not supported by this backend".to_string(),
        ))
    }
    fn get_task_artifacts(&self, id: &str) -> Result<Option<Vec<TaskArtifact>>, OrbitError>;
    fn get_task_artifact(
        &self,
        _id: &str,
        _path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        Err(OrbitError::Store(
            "task artifact read is not supported by this backend".to_string(),
        ))
    }
    /// Read one artifact from a manifest entry the caller already holds.
    ///
    /// The file store reads that blob under the bundle lock and checks its
    /// size and sha256, without hashing the rest of the bundle. Backends
    /// without that path fall back to [`Self::get_task_artifact`].
    fn read_manifest_artifact(
        &self,
        id: &str,
        file: &ArtifactManifestFileV2,
    ) -> Result<TaskArtifact, OrbitError> {
        self.get_task_artifact(id, &file.path)?.ok_or_else(|| {
            OrbitError::not_found(NotFoundKind::Artifact, format!("{id}/{}", file.path))
        })
    }
    fn upsert_task_artifacts(
        &self,
        id: &str,
        params: TaskArtifactUpdateParams,
    ) -> Result<(), OrbitError>;
}

pub trait TaskReservationStoreBackend: Send + Sync {
    /// Read active path holders, task reservations and hard leases, without
    /// expiring or otherwise mutating rows. Advisory leases hold nothing.
    fn inspect_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<ActiveTaskReservation>, OrbitError>;

    /// Read one active reservation by id with the visibility of
    /// [`Self::inspect_active_task_reservations`]; `None` when it is gone,
    /// released, expired, or outside the workspace.
    fn inspect_active_task_reservation(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
        reservation_id: &str,
    ) -> Result<Option<ActiveTaskReservation>, OrbitError>;

    fn list_active_task_reservations(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<TaskReservationListResult, OrbitError>;

    /// Read every task reservation, active or released, oldest first; leases
    /// are not task reservations.
    /// Read-only: it neither expires nor releases rows.
    fn list_task_reservation_history(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<TaskReservationRecord>, OrbitError>;

    fn check_task_reservation_conflicts(
        &self,
        params: TaskReservationCheckParams,
    ) -> Result<TaskReservationCheckResult, OrbitError>;

    fn reserve_task_reservation(
        &self,
        params: TaskReservationReserveParams,
    ) -> Result<TaskReservationReserveResult, OrbitError>;

    fn release_task_reservation(
        &self,
        params: TaskReservationReleaseParams,
    ) -> Result<TaskReservationReleaseResult, OrbitError>;

    fn release_task_reservations_by_owner_run_id(
        &self,
        params: TaskReservationReleaseByOwnerParams,
    ) -> Result<TaskReservationReleaseByOwnerResult, OrbitError>;

    fn list_owned_task_reservation_conflicts(
        &self,
        params: TaskReservationOwnedConflictsParams,
    ) -> Result<TaskReservationOwnedConflictsResult, OrbitError>;

    /// Record a path lease, granted or queued, or report the holders that
    /// refuse a hard one asked not to wait.
    fn insert_lease(&self, params: LeaseInsertParams) -> Result<LeaseInsertResult, OrbitError>;

    /// Add selectors to a granted lease; blocked ones on a hard lease queue.
    fn expand_lease(&self, params: LeaseExpandParams) -> Result<LeaseExpandResult, OrbitError>;

    /// Grant waiting requests and expansions, in queue order, whose paths
    /// are free.
    fn grant_queued_leases(
        &self,
        params: LeaseQueueGrantParams,
    ) -> Result<LeaseQueueGrantResult, OrbitError>;

    /// Move an active lease's `expires_at` forward.
    fn renew_lease(&self, params: LeaseRenewParams) -> Result<LeaseRenewResult, OrbitError>;

    /// End an active lease.
    fn release_lease(&self, params: LeaseReleaseParams) -> Result<LeaseReleaseResult, OrbitError>;

    /// Read one lease in any state, without mutating rows.
    fn get_lease(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
        lease_id: &str,
    ) -> Result<Option<LeaseRecord>, OrbitError>;

    /// The workspace's active leases, advisory and hard, without expiring
    /// or otherwise mutating rows.
    fn inspect_active_leases(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<Vec<LeaseRecord>, OrbitError>;

    /// The workspace's active leases after lazy expiry.
    fn list_active_leases(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<LeaseListResult, OrbitError>;

    /// Expire elapsed leases and publish one 80%-TTL warning per heartbeat.
    /// Returns the remaining active leases for the owner's landing checks.
    fn sweep_lease_lifetimes(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<LeaseListResult, OrbitError>;

    /// Durable notices strictly after a cursor, optionally for one lease.
    fn list_lease_notices(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
        lease_id: Option<&str>,
        after: u64,
        limit: u32,
    ) -> Result<Vec<crate::contracts::LeaseNotice>, OrbitError>;

    /// Take the exclusive workspace claim [ADR-0352, ORB-10709], or report the
    /// incumbent that refused it.
    fn acquire_workspace_claim(
        &self,
        params: WorkspaceClaimAcquireParams,
    ) -> Result<WorkspaceClaimAcquireResult, OrbitError>;

    /// Release the claim with its token, or force-release it.
    fn release_workspace_claim(
        &self,
        params: WorkspaceClaimReleaseParams,
    ) -> Result<WorkspaceClaimReleaseResult, OrbitError>;

    /// The active claim after lazy expiry, or `None` when unclaimed.
    fn show_workspace_claim(
        &self,
        workspace_orbit_dir: &str,
        workspace_id: Option<&str>,
    ) -> Result<WorkspaceClaimStatusResult, OrbitError>;

    /// Whether a presented token satisfies the active claim. The comparison
    /// stays inside the store so a refusal never has to carry the incumbent's
    /// token back out.
    fn check_workspace_claim(
        &self,
        params: WorkspaceClaimCheckParams,
    ) -> Result<WorkspaceClaimCheckResult, OrbitError>;
}
