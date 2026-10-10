mod relations;

use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use orbit_types::task::{TaskEnvelopeV2, TaskPriority, TaskRelation, TaskStatus, TaskType};
use tempfile::TempDir;

use super::{TaskRegistryStore, task_registry_path};

fn registry_path(temp: &TempDir) -> PathBuf {
    task_registry_path(temp.path())
}

/// ORB-15070: a fresh open through a symlinked root must key the same
/// `workspaces_dir` as a later open of the canonical root. The repair gate
/// keys on that path, so a spelling split splits its attempt budget.
/// Admitted under criterion 3 (symlink confinement): `workspaces_dir` is
/// crate-private, so the public boundary cannot observe the key split.
#[cfg(unix)]
#[test]
fn fresh_open_through_symlinked_root_matches_reopen_by_canonical_root() {
    let real = TempDir::new().expect("tempdir");
    let alias_parent = TempDir::new().expect("tempdir");
    let alias = alias_parent.path().join("root");
    std::os::unix::fs::symlink(real.path(), &alias).expect("symlink root");

    let fresh = TaskRegistryStore::open(&task_registry_path(&alias)).expect("fresh open");
    let reopened = TaskRegistryStore::open(&registry_path(&real)).expect("reopen");

    assert_eq!(
        fresh.workspaces_dir(),
        reopened.workspaces_dir(),
        "a fresh and a reopened handle on one registry root must share one workspaces_dir"
    );
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
        crew_source: None,
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
