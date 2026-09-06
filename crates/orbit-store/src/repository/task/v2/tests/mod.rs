use chrono::{TimeZone, Utc};
use orbit_types::task::{
    ExternalRef, TaskArtifact, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
};
use tempfile::TempDir;

use super::*;
use crate::contracts::{
    TaskArtifactUpdateParams, TaskCreateParams, TaskDocumentUpdateParams, TaskHistoryUpdateParams,
};
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};

pub(super) fn store(temp: &TempDir) -> TaskV2Store {
    let registry =
        TaskRegistryStore::open(&task_registry_path(temp.path())).expect("open registry");
    let repo_dir = temp.path().join("repo");
    let orbit_dir = repo_dir.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            workspace_id: Some("orbit-test-123456".to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: repo_dir.clone(),
            workspace_path: repo_dir.clone(),
            orbit_dir: orbit_dir.clone(),
            repo_fingerprint: None,
        })
        .expect("bind workspace");
    TaskV2Store::new(
        registry,
        binding.workspace_id,
        orbit_dir,
        Some(repo_dir.to_string_lossy().into_owned()),
        Some(repo_dir.to_string_lossy().into_owned()),
    )
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
        workspace_path: None,
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

#[cfg(unix)]
pub(super) struct RestoreWritePerms<'a> {
    path: &'a std::path::Path,
}

#[cfg(unix)]
impl Drop for RestoreWritePerms<'_> {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(self.path, std::fs::Permissions::from_mode(0o755));
    }
}

#[cfg(unix)]
pub(super) fn make_readonly(path: &std::path::Path) -> RestoreWritePerms<'_> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path).expect("meta").permissions();
    perms.set_mode(0o555);
    std::fs::set_permissions(path, perms).expect("chmod -w");
    RestoreWritePerms { path }
}

pub(super) fn assert_sandbox_write_io(err: &OrbitError, path_substr: &str) {
    match err {
        OrbitError::Io(message) => {
            assert!(
                message.contains(path_substr),
                "expected path `{path_substr}` in `{message}`"
            );
            assert!(
                message.contains("is not writable"),
                "expected writable attribution in `{message}`"
            );
            assert!(
                message.contains("sandbox or environment"),
                "expected sandbox/environment hint in `{message}`"
            );
            assert!(
                message.contains("not an Orbit store defect"),
                "expected store-defect negation in `{message}`"
            );
        }
        other => panic!("expected Io, got {other}"),
    }
}

mod concurrency;
mod crud;
mod update;

mod listing;

mod listing_bench;
