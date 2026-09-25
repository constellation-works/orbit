// Content moved from tests.rs per ORB-00231

mod allocator;
mod bindings;
mod index;
mod listing;
mod read_pool;
mod relations;
mod schema;
mod store;
mod workspace_config;
mod workspaces;

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{TimeZone, Utc};
use orbit_types::task::{TaskEnvelopeV2, TaskPriority, TaskRelation, TaskStatus, TaskType};
use rusqlite::Connection;
use tempfile::TempDir;

use super::{BindWorkspaceParams, TaskRegistryStore, WorkspaceCheckoutBinding, task_registry_path};

fn registry_path(temp: &TempDir) -> PathBuf {
    task_registry_path(temp.path())
}

fn store(temp: &TempDir) -> TaskRegistryStore {
    TaskRegistryStore::open(&registry_path(temp)).expect("open registry")
}

fn bind(store: &TaskRegistryStore, root: &Path) -> WorkspaceCheckoutBinding {
    let orbit_dir = root.join(".orbit");
    fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    store
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some("orbit-test-123456".into()),
            slug: "Orbit Test".into(),
            repo_root: root.to_path_buf(),
            workspace_path: root.to_path_buf(),
            orbit_dir,
            repo_fingerprint: None,
        })
        .expect("bind workspace")
}

fn create_canonical_bundle(
    store: &TaskRegistryStore,
    workspace: &WorkspaceCheckoutBinding,
    task_id: &str,
) -> PathBuf {
    let bundle_dir = store
        .canonical_task_bundle_path(&workspace.partition_id, task_id)
        .expect("canonical bundle path");
    fs::create_dir_all(&bundle_dir).expect("create bundle");
    bundle_dir
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

fn table_columns(conn: &Connection, table: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table info");
    stmt.query_map([], |row| row.get::<_, String>(1))
        .expect("query table info")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect columns")
}
