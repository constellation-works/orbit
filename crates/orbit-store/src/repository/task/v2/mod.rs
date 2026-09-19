//! Task bundle v2 persistence is split into focused submodules by operation surface.
//! The `crud` module owns task creation, listing, filtering, searching, and deletion.
//! The `updates` module owns document and history mutations.
//! The `artifacts` module owns task artifact reads, manifests, and upserts.
//! The `sidecars` module owns comments and history row reads.
//! The `index` module owns generated index reads, rebuilds, bundle translation, and task locking helpers.
//! The `envelope_cache` module owns freshness-stamped reuse of parsed envelopes.
//! The `query` module owns in-memory, sidecar, and artifact query matching.
//! The `relations` module owns relation construction and replacement helpers.
//! The `sequencing` module owns monotonic event and comment sequence calculations.
//! The `artifact_paths` module owns artifact path normalization and safe resolution.
//! The `acceptance` module owns acceptance-criteria rendering and parsing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use orbit_common::fs::io::atomic_write_bytes;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::identity::OrbitId;
use orbit_types::task::{
    ArtifactManifestFileV2, ArtifactManifestV2, ExternalRef, TASK_ARTIFACT_FILES_DIR_NAME,
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_ARTIFACTS_DIR_NAME, Task, TaskArtifact, TaskComment,
    TaskCommentRowV2, TaskEnvelopeV2, TaskEventRowV2, TaskHistoryEntry, TaskPriority, TaskRelation,
    TaskRelationType, TaskStatus, normalize_task_tags, validate_relative_artifact_path,
};
use sha2::{Digest, Sha256};

use crate::contracts::{
    RegisteredTaskResolution, TaskArtifactUpdateParams, TaskCreateParams, TaskDocumentUpdateParams,
    TaskHistoryUpdateParams,
};
use crate::driver::file::sort::sort_by_created_desc_id_asc;
use crate::driver::sqlite::task_registry::{TaskIndexFilter, TaskRegistryStore};
use crate::repository::task::coordination::TaskCommitBoundary;
use crate::repository::task::v2_bundle::{TaskBundleStoreV2, TaskBundleV2, TaskDocumentV2};

mod acceptance;
mod artifact_paths;
mod artifacts;
mod crud;
mod envelope_cache;
mod index;
mod listing;
mod query;
mod relations;
pub(crate) mod sequencing;
mod sidecars;
mod updates;

#[cfg(test)]
mod tests;

use acceptance::{parse_acceptance, render_acceptance};
use artifact_paths::{normalize_v2_artifact_path, resolve_v2_artifact_file_path};
use envelope_cache::EnvelopeCache;
use relations::{relations_from_create_params, replace_relations};
use sequencing::{next_event_id, next_sequence};

pub(crate) struct TaskV2Store {
    registry: TaskRegistryStore,
    bundle_store: TaskBundleStoreV2,
    workspace_id: String,
    /// Envelope parses reused across listings; see [`envelope_cache`].
    envelope_cache: EnvelopeCache,
    /// The partition's durable commit boundary, when this store was composed
    /// with one ([`crate::compose::workspace_coordinated_backends`]).
    ///
    /// With it, every ordinary mutation runs inside the boundary and every
    /// read settles an interrupted commit before exposing state, so an
    /// admission decision can read readiness and publish without a task write
    /// slipping in between. Legacy composition retains per-bundle locking,
    /// but is refused once this partition has activated coordination.
    coordination: Option<Arc<TaskCommitBoundary>>,
}

impl TaskV2Store {
    pub(crate) fn claim_boundary(&self) -> Result<&TaskCommitBoundary, OrbitError> {
        self.coordination
            .as_deref()
            .ok_or_else(|| OrbitError::Store("claim lifecycle unavailable".into()))
    }

    pub(crate) fn new(registry: TaskRegistryStore, workspace_id: String) -> Self {
        Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            registry,
            workspace_id,
            envelope_cache: EnvelopeCache::default(),
            coordination: None,
        }
    }

    /// The same store, participating in one partition's commit boundary.
    pub(crate) fn with_commit_boundary(
        registry: TaskRegistryStore,
        workspace_id: String,
        coordination: Arc<TaskCommitBoundary>,
    ) -> Self {
        Self {
            coordination: Some(coordination),
            ..Self::new(registry, workspace_id)
        }
    }

    /// Run an ordinary mutation inside the boundary. Legacy stores acquire
    /// the same locks and refuse partitions requiring coordinated backends.
    pub(crate) fn in_boundary<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        match &self.coordination {
            Some(boundary) => boundary.enter_ordinary(op),
            None => TaskCommitBoundary::enter_uncoordinated(&self.registry, &self.workspace_id, op),
        }
    }

    /// Settle an interrupted commit before a read exposes task state. One
    /// existence check when nothing is pending.
    pub(super) fn ensure_recovered(&self) -> Result<(), OrbitError> {
        match &self.coordination {
            Some(boundary) => boundary.recover_if_pending(),
            None => {
                TaskCommitBoundary::enter_uncoordinated(&self.registry, &self.workspace_id, || {
                    Ok(())
                })
            }
        }
    }
}
