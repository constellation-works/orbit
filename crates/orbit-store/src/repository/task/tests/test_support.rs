// Bundle fixtures for the bundle_io tests, which need a bound `TaskBundleStoreV2`.

use std::fs;

use chrono::{TimeZone, Utc};
use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TaskCommentRowV2, TaskEnvelopeV2, TaskEventRowV2, TaskPriority,
    TaskStatus, TaskType,
};
use tempfile::TempDir;

use super::super::v2_bundle::*;
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};

pub(crate) fn sample_bundle(id: &str) -> TaskBundleV2 {
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap();
    TaskBundleV2 {
        envelope: TaskEnvelopeV2 {
            job_run_machine: None,
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            id: id.to_string(),
            title: "Build v2 bundle store".to_string(),
            status: TaskStatus::Backlog,
            task_type: TaskType::Feature,
            priority: TaskPriority::High,
            complexity: None,
            pr_status: None,
            job_run_id: None,
            crew: None,
            crew_source: None,
            orchestrator: None,
            relations: Vec::new(),
            tags: vec!["task-artifacts".to_string()],
            required_tools: Vec::new(),
            context_files: vec!["docs/design/task-artifacts/2_design.md".to_string()],
            external_refs: Vec::new(),
            created_by: Some("codex:gpt-5.5".to_string()),
            planned_by: None,
            implemented_by: None,
            created_at: now,
            updated_at: now,
        },
        description: "Description body".to_string(),
        acceptance: "- [ ] Bundle writes are durable".to_string(),
        plan: "1. Write bundle".to_string(),
        execution_summary: String::new(),
        events: vec![TaskEventRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            event_id: "EV-0001".to_string(),
            at: now,
            by: "codex:gpt-5.5".to_string(),
            event_type: "created".to_string(),
            note: None,
            from_status: None,
            to_status: Some(TaskStatus::Backlog),
        }],
        comments: vec![TaskCommentRowV2 {
            schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
            comment_id: "C-0001".to_string(),
            at: now,
            by: "daniel".to_string(),
            body: "Looks good.".to_string(),
        }],
        artifact_manifest: None,
    }
}

pub(crate) fn bundle_store(temp: &TempDir) -> TaskBundleStoreV2 {
    let registry =
        TaskRegistryStore::open(&task_registry_path(temp.path())).expect("open registry");
    let orbit_dir = temp.path().join("repo").join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("orbit-test-123456".to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: temp.path().join("repo"),
            workspace_path: temp.path().join("repo"),
            orbit_dir: orbit_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("bind workspace");
    TaskBundleStoreV2::new(registry, binding.partition_id)
}
