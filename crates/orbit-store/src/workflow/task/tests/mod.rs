use std::path::Path;

use chrono::{TimeZone, Utc};
use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TaskEnvelopeV2, TaskEventRowV2, TaskPriority, TaskRelation,
    TaskStatus, TaskType,
};

use crate::driver::file::task_bundle::TaskBundleV2;
use crate::driver::sqlite::task_registry::{
    TaskRegistryStore, WorkspaceCheckoutBinding, task_registry_path,
};
use crate::repository::task::v2_bundle::TaskBundleStoreV2;

pub(super) mod archive;
mod git;
mod publish;

fn open_registry(global: &Path) -> TaskRegistryStore {
    TaskRegistryStore::open(&task_registry_path(global)).expect("open registry")
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
            crew_source: None,
            orchestrator: Some("archive-orchestrator".to_string()),
            origin: None,
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
            operation_id: None,
            decision: None,
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
