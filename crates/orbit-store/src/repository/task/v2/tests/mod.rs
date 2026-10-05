use chrono::{TimeZone, Utc};
use orbit_types::task::{ExternalRef, TaskComment, TaskPriority, TaskStatus, TaskType};
use tempfile::TempDir;

use super::*;
use crate::contracts::{TaskCreateParams, TaskHistoryUpdateParams};
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};

pub(super) fn store(temp: &TempDir) -> TaskV2Store {
    let registry =
        TaskRegistryStore::open(&task_registry_path(temp.path())).expect("open registry");
    bound_store(&registry, temp, "orbit-test-123456", "repo")
}

/// A second (third, ...) workspace bound into the same coordination registry,
/// as two checkouts on one machine are. Only the partition and its checkout
/// directory differ; task ids stay globally allocated across both.
pub(super) fn bound_store(
    registry: &TaskRegistryStore,
    temp: &TempDir,
    partition_id: &str,
    checkout: &str,
) -> TaskV2Store {
    bound_store_at(registry, temp.path(), partition_id, checkout)
}

/// [`bound_store`] with the checkout under an explicit `root`, for fixtures
/// that need a root other than a temp directory's own path.
pub(super) fn bound_store_at(
    registry: &TaskRegistryStore,
    root: &std::path::Path,
    partition_id: &str,
    checkout: &str,
) -> TaskV2Store {
    let repo_dir = root.join(checkout);
    let orbit_dir = repo_dir.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition_id.to_string()),
            slug: format!("Orbit Test {checkout}"),
            repo_root: repo_dir.clone(),
            workspace_path: repo_dir.clone(),
            orbit_dir: orbit_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("bind workspace");
    TaskV2Store::new(registry.clone(), binding.partition_id)
}

pub(super) fn create_params(title: &str, status: TaskStatus) -> TaskCreateParams {
    let now = Utc.with_ymd_and_hms(2026, 5, 11, 12, 0, 0).unwrap();
    TaskCreateParams {
        actor: "codex:gpt-5.5".to_string(),
        parent_id: None,
        title: title.to_string(),
        description: "Detailed task description".to_string(),
        acceptance_criteria: vec![
            "First criterion".to_string(),
            "Second criterion".to_string(),
        ],
        dependencies: Vec::new(),
        relations: Vec::new(),
        tags: vec!["task-artifacts".to_string(), "v2".to_string()],
        required_tools: Vec::new(),
        plan: "1. Do the work".to_string(),
        execution_summary: String::new(),
        context_files: vec!["docs/design/task-artifacts/1_overview.md".to_string()],
        repo_root: None,
        created_by: Some("codex:gpt-5.5".to_string()),
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::High,
        complexity: None,
        task_type: TaskType::Feature,
        external_refs: vec![
            ExternalRef::try_new("linear".to_string(), "ENG-123".to_string(), None).unwrap(),
        ],
        source_task_id: None,
        crew: None,
        orchestrator: None,
        comments: vec![TaskComment {
            at: now,
            by: "daniel".to_string(),
            message: "Please build this.".to_string(),
        }],
    }
}

mod concurrency;
mod repair_gate;
