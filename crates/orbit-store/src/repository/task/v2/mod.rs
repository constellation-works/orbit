//! Task bundle v2 persistence is split into focused submodules by operation surface.
//! The `crud` module owns task creation, listing, filtering, searching, and deletion.
//! The `updates` module owns document and history mutations.
//! The `artifacts` module owns task artifact reads, manifests, and upserts.
//! The `sidecars` module owns comments and history row reads.
//! The `index` module owns generated index reads, rebuilds, bundle translation, and task locking helpers.
//! The `query` module owns in-memory, sidecar, and artifact query matching.
//! The `relations` module owns relation construction and replacement helpers.
//! The `sequencing` module owns monotonic event and comment sequence calculations.
//! The `artifact_paths` module owns artifact path normalization and safe resolution.
//! The `acceptance` module owns acceptance-criteria rendering and parsing.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use orbit_common::fs::io::{atomic_write_bytes, with_exclusive_file_lock};
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
    TaskArtifactUpdateParams, TaskCreateParams, TaskDocumentUpdateParams, TaskHistoryUpdateParams,
};
use crate::driver::file::sort::sort_by_created_desc_id_asc;
use crate::driver::sqlite::task_registry::{TaskIndexFilter, TaskRegistryStore};
use crate::repository::task::v2_bundle::{TaskBundleStoreV2, TaskBundleV2, TaskDocumentV2};

mod acceptance;
mod artifact_paths;
mod artifacts;
mod crud;
mod index;
mod listing;
mod query;
mod relations;
mod sequencing;
mod sidecars;
mod updates;

#[cfg(test)]
mod tests;

use acceptance::{parse_acceptance, render_acceptance};
use artifact_paths::{normalize_v2_artifact_path, resolve_v2_artifact_file_path};
use relations::{relations_from_create_params, replace_relations};
use sequencing::{next_event_id, next_sequence};

pub(crate) struct TaskV2Store {
    registry: TaskRegistryStore,
    bundle_store: TaskBundleStoreV2,
    workspace_id: String,
}

impl TaskV2Store {
    pub(crate) fn new(
        registry: TaskRegistryStore,
        workspace_id: String,
        workspace_orbit_dir: PathBuf,
        _workspace_path: Option<String>,
        _repo_root: Option<String>,
    ) -> Self {
        Self {
            bundle_store: TaskBundleStoreV2::new(
                registry.clone(),
                workspace_id.clone(),
                workspace_orbit_dir,
            ),
            registry,
            workspace_id,
        }
    }

    pub(crate) fn new_checkoutless(registry: TaskRegistryStore, workspace_id: String) -> Self {
        Self {
            bundle_store: TaskBundleStoreV2::new_checkoutless(
                registry.clone(),
                workspace_id.clone(),
            ),
            registry,
            workspace_id,
        }
    }
}
