mod relations;

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use orbit_types::task::{TaskEnvelopeV2, TaskPriority, TaskRelation, TaskStatus, TaskType};
use tempfile::TempDir;

use super::{TaskRegistryStore, task_registry_path};

fn registry_path(temp: &TempDir) -> PathBuf {
    task_registry_path(temp.path())
}

fn store(temp: &TempDir) -> TaskRegistryStore {
    TaskRegistryStore::open(&registry_path(temp)).expect("open registry")
}

fn envelope(
    task_id: &str,
    status: TaskStatus,
    tags: Vec<String>,
    relations: Vec<TaskRelation>,
) -> TaskEnvelopeV2 {
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap();
    TaskEnvelopeV2 {
        job_run_machine: None,
        schema_version: orbit_types::task::TASK_ARTIFACT_SCHEMA_VERSION,
        id: task_id.to_string(),
        title: format!("Task {task_id}"),
        status,
        task_type: TaskType::Feature,
        priority: TaskPriority::High,
        complexity: None,
        pr_status: None,
        job_run_id: None,
        crew: None,
        orchestrator: None,
        relations,
        tags,
        required_tools: Vec::new(),
        context_files: Vec::new(),
        external_refs: Vec::new(),
        created_by: Some("codex:gpt-5.5".to_string()),
        planned_by: None,
        implemented_by: None,
        created_at: now,
        updated_at: now,
    }
}
