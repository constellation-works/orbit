use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::task::{ArtifactManifestFileV2, TaskArtifact};

use crate::boundary::TaskV2Store;
use crate::contract::TaskArtifactUpdateParams;

/// Artifact records of a workspace's task store.
#[derive(Clone)]
pub struct TaskArtifactRepository {
    pub(crate) store: Arc<TaskV2Store>,
}

impl TaskArtifactRepository {
    /// Artifact manifest for a task, if any.
    pub fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Option<Vec<ArtifactManifestFileV2>>, OrbitError> {
        self.store
            .in_boundary(|| self.store.get_task_artifact_manifest(id))
    }

    pub fn get_task_artifacts(&self, id: &str) -> Result<Option<Vec<TaskArtifact>>, OrbitError> {
        self.store.in_boundary(|| self.store.get_task_artifacts(id))
    }

    /// One artifact payload by path, if present.
    pub fn get_task_artifact(
        &self,
        id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        self.store
            .in_boundary(|| self.store.get_task_artifact(id, path))
    }

    /// One artifact payload from a settled bundle, without coordination or
    /// recovery. Unreadable bundles still produce errors.
    pub fn observe_task_artifact(
        &self,
        id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        orbit_types::task::validate_orb_task_id(id)?;
        self.store.observe_task_artifact(id, path)
    }

    /// Read one artifact from a manifest entry the caller already holds.
    pub fn read_manifest_artifact(
        &self,
        id: &str,
        file: &ArtifactManifestFileV2,
    ) -> Result<TaskArtifact, OrbitError> {
        self.store
            .in_boundary(|| self.store.read_manifest_artifact(id, file))
    }

    pub fn upsert_task_artifacts(
        &self,
        id: &str,
        params: TaskArtifactUpdateParams,
    ) -> Result<(), OrbitError> {
        self.store.upsert_task_artifacts(id, &params)
    }
}
