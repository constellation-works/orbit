// Test support helpers extracted from the old nested v2_bundle/tests/test_support.rs
// and shared by v2_bundle sibling tests + the internal bundle_io tests.
// Promoted here per docs/design-patterns/test_layout.md guidance for shared test helpers.

use std::fs;
use std::path::{Path, PathBuf};

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

pub(crate) fn task_lock_path(bundle_dir: &Path) -> PathBuf {
    let file_name = bundle_dir
        .file_name()
        .and_then(|value| value.to_str())
        .expect("bundle path has file name");
    bundle_dir.with_file_name(format!(".{file_name}.lock"))
}

pub(crate) fn legacy_double_dot_lock_path(bundle_dir: &Path, task_id: &str) -> PathBuf {
    bundle_dir.with_file_name(format!("..{task_id}.create.lock"))
}

pub(crate) fn lock_entries_for_task(tasks_dir: &Path, task_id: &str) -> Vec<String> {
    let mut entries = fs::read_dir(tasks_dir)
        .expect("read task workspace dir")
        .map(|entry| {
            entry
                .expect("read task workspace entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains(task_id) && name.ends_with(".lock"))
        .collect::<Vec<_>>();
    entries.sort();
    entries
}
