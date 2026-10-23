use crate::contracts::params::*;
use orbit_common::OrbitError;
use orbit_types::task::{ArtifactManifestFileV2, TaskArtifact, TaskComment, TaskHistoryEntry};

pub trait TaskDocumentStoreBackend: Send + Sync {
    fn update_task_document(
        &self,
        id: &str,
        params: TaskDocumentUpdateParams,
    ) -> Result<(), OrbitError>;
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
    fn update_task_history(
        &self,
        id: &str,
        params: TaskHistoryUpdateParams,
    ) -> Result<(), OrbitError>;
}

pub trait TaskArtifactStoreBackend: Send + Sync {
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
    fn upsert_task_artifacts(
        &self,
        id: &str,
        params: TaskArtifactUpdateParams,
    ) -> Result<(), OrbitError>;
}

pub trait TaskReservationStoreBackend: Send + Sync {
    /// Read active reservations without expiring or otherwise mutating rows.
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

    /// Read every file reservation, active or released, oldest first.
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
