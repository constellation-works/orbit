use std::fs;
use std::path::Path;

use chrono::{TimeZone, Utc};
use orbit_types::task::{
    ArtifactManifestFileV2, TASK_ARTIFACT_FILES_DIR_NAME, TASK_ARTIFACT_SCHEMA_VERSION,
    TASK_ARTIFACTS_DIR_NAME, TaskEnvelopeV2, TaskEventRowV2, TaskPriority, TaskRelation,
    TaskStatus, TaskType,
};
use sha2::{Digest, Sha256};

use crate::driver::file::task_bundle::TaskBundleV2;
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, WorkspaceCheckoutBinding, task_registry_path,
};
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

use super::*;

mod import;
mod publish;

fn open_registry(global: &Path) -> TaskRegistryStore {
    TaskRegistryStore::open(&task_registry_path(global)).expect("open registry")
}

fn bind(registry: &TaskRegistryStore, global: &Path, ws_id: &str) -> WorkspaceCheckoutBinding {
    let orbit_dir = global.join("repos").join(ws_id).join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(ws_id.to_string()),
            slug: "sample".to_string(),
            repo_root: orbit_dir.parent().unwrap().to_path_buf(),
            workspace_path: orbit_dir.parent().unwrap().to_path_buf(),
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind workspace")
}

fn bundle_store(
    registry: &TaskRegistryStore,
    binding: &WorkspaceCheckoutBinding,
) -> TaskBundleStoreV2 {
    TaskBundleStoreV2::new(registry.clone(), binding.partition_id.clone())
}

fn make_bundle(id: &str, title: &str, relations: Vec<TaskRelation>) -> TaskBundleV2 {
    let now = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
    TaskBundleV2 {
        envelope: TaskEnvelopeV2 {
            job_run_machine: None,
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            id: id.to_string(),
            title: title.to_string(),
            status: TaskStatus::Backlog,
            task_type: TaskType::Feature,
            priority: TaskPriority::High,
            complexity: None,
            pr_status: None,
            job_run_id: None,
            crew: None,
            orchestrator: Some("archive-orchestrator".to_string()),
            relations,
            tags: vec!["migration".to_string()],
            required_tools: Vec::new(),
            context_files: Vec::new(),
            external_refs: Vec::new(),
            created_by: Some("codex".to_string()),
            planned_by: None,
            implemented_by: None,
            created_at: now,
            updated_at: now,
        },
        description: format!("description for {id}"),
        acceptance: "- [ ] done".to_string(),
        plan: "plan".to_string(),
        execution_summary: String::new(),
        events: vec![TaskEventRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            event_id: "EV-0001".to_string(),
            at: now,
            by: "codex".to_string(),
            event_type: "created".to_string(),
            note: None,
            from_status: None,
            to_status: Some(TaskStatus::Backlog),
        }],
        comments: Vec::new(),
        artifact_manifest: None,
    }
}

/// Write a bundle to disk and register+index it (no allocator advance — ids are
/// chosen explicitly by the test).
fn seed(store: &TaskBundleStoreV2, registry: &TaskRegistryStore, ws: &str, bundle: &TaskBundleV2) {
    store.create_bundle(bundle).expect("create bundle");
    registry
        .replace_task_index(ws, &bundle.envelope)
        .expect("index bundle");
}

fn exported_at() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 4, 0, 0, 0).unwrap()
}

/// Seed a blob at `path` (relative to the bundle's `artifacts/files/` dir) and
/// return the manifest entry describing it. Callers must merge the returned
/// entries into a single `ArtifactManifestV2` and `rewrite_artifact_manifest`
/// so `read_bundle_at` accepts the bundle.
fn seed_artifact_blob(
    store: &TaskBundleStoreV2,
    task_id: &str,
    path: &str,
    bytes: &[u8],
    actor: &str,
) -> ArtifactManifestFileV2 {
    let bundle_dir = store.bundle_path(task_id).expect("bundle path");
    let blob = format!("{TASK_ARTIFACT_FILES_DIR_NAME}/{path}");
    let blob_path = bundle_dir.join(TASK_ARTIFACTS_DIR_NAME).join(&blob);
    if let Some(parent) = blob_path.parent() {
        fs::create_dir_all(parent).expect("create artifact parent");
    }
    fs::write(&blob_path, bytes).expect("write blob");
    ArtifactManifestFileV2 {
        origin: None,
        path: path.to_string(),
        blob,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        media_type: "application/octet-stream".to_string(),
        size_bytes: bytes.len() as u64,
        created_by: actor.to_string(),
        created_at: Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap(),
    }
}
