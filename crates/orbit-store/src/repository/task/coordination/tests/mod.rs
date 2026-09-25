//! The commit boundary's durability contract.
//!
//! `mod.rs` holds the shared fixture; `commit.rs` covers atomic publication;
//! `recovery.rs` covers interrupted commits and fail-closed compensation;
//! `serialization.rs` covers the boundary shared by ordinary mutations across
//! store instances and processes. The remaining files mirror their source
//! files.

use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_types::task::{
    Task, TaskComplexity, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
};
use tempfile::TempDir;

use super::*;
use crate::compose::{CoordinatedWorkspaceBackends, workspace_coordinated_backends};
use crate::contracts::{
    ActiveTaskReservation, TaskCoordinationCommit, TaskCoordinationCommitOutcome,
    TaskCoordinationCommitParams, TaskCoordinationRow, TaskCreateParams,
    TaskReservationReserveParams,
};
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};

mod admission;
mod commit;
mod handoff;
mod landing;
mod lifecycle;
mod recovery;
mod serialization;

const PARTITION_ID: &str = "orbit-test-123456";

/// One coordinated composition over a root directory. Opening a second one on
/// the same root is exactly what a second process (or a restart) does.
pub(super) struct Coordinated {
    pub(super) backends: CoordinatedWorkspaceBackends,
    pub(super) orbit_dir: PathBuf,
}

impl Coordinated {
    pub(super) fn open(root: &Path) -> Self {
        let registry = TaskRegistryStore::open(&task_registry_path(root)).expect("open registry");
        let repo_dir = root.join("repo");
        let orbit_dir = repo_dir.join(".orbit");
        std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
        let binding = registry
            .bind_workspace(BindWorkspaceParams {
                partition_id: Some(PARTITION_ID.to_string()),
                slug: "Orbit Test".to_string(),
                repo_root: repo_dir.clone(),
                workspace_path: repo_dir.clone(),
                orbit_dir: orbit_dir.clone(),
                repo_fingerprint: None,
            })
            .expect("bind workspace");
        let store = Store::open(&root.join("state.sqlite")).expect("open store");
        let backends =
            workspace_coordinated_backends(registry, binding.partition_id, store).expect("compose");
        Self {
            backends,
            orbit_dir,
        }
    }

    pub(super) fn boundary(&self) -> &TaskCommitBoundary {
        &self.backends.commit_boundary
    }

    pub(super) fn create_task(&self, title: &str) -> Task {
        self.backends
            .task
            .task
            .create_task(create_params(title))
            .expect("create task")
    }

    pub(super) fn task(&self, id: &str) -> Task {
        self.backends
            .task
            .task
            .get_task(id)
            .expect("read task")
            .expect("task exists")
    }

    pub(super) fn history(&self, id: &str) -> Vec<TaskHistoryEntry> {
        self.backends
            .task
            .history
            .get_task_history(id)
            .expect("read history")
            .expect("task exists")
    }

    pub(super) fn active_reservations(&self) -> Vec<ActiveTaskReservation> {
        self.backends
            .reservation
            .inspect_active_task_reservations(&self.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
            .expect("read reservations")
    }

    pub(super) fn reservation_params(
        &self,
        task_id: &str,
        file: &str,
    ) -> TaskReservationReserveParams {
        TaskReservationReserveParams {
            workspace_orbit_dir: self.orbit_dir.to_string_lossy().to_string(),
            workspace_id: Some(PARTITION_ID.to_string()),
            task_ids: vec![task_id.to_string()],
            requested_files: vec![file.to_string()],
            actor: "codex".to_string(),
            ttl_seconds: 600,
            owner_run_id: None,
            owner_metadata_json: None,
        }
    }

    /// The shape an admission decision commits: a `backlog → in-progress`
    /// transition, its history, a reservation over the task's own files, and
    /// one dependent coordination row.
    pub(super) fn admission_params(
        &self,
        task_id: &str,
        file: &str,
    ) -> TaskCoordinationCommitParams {
        TaskCoordinationCommitParams {
            task_id: task_id.to_string(),
            actor: "codex".to_string(),
            expected_status: vec![TaskStatus::Backlog],
            status: Some(TaskStatus::InProgress),
            status_event: Some("pulled_by".to_string()),
            status_note: Some("machine=dk-server-1".to_string()),
            append_history: Vec::new(),
            reservation: Some(self.reservation_params(task_id, file)),
            rows: vec![TaskCoordinationRow {
                kind: "admission-receipt".to_string(),
                row_id: format!("request-{task_id}"),
                payload_json: "{\"idle\":false}".to_string(),
            }],
        }
    }
}

pub(super) fn create_params(title: &str) -> TaskCreateParams {
    TaskCreateParams {
        actor: "codex".to_string(),
        parent_id: None,
        title: title.to_string(),
        description: "Detailed task description".to_string(),
        acceptance_criteria: vec!["First criterion".to_string()],
        dependencies: Vec::new(),
        relations: Vec::new(),
        tags: vec!["distributed-drain".to_string()],
        required_tools: Vec::new(),
        plan: "1. Do the work".to_string(),
        execution_summary: String::new(),
        context_files: vec!["src/lib.rs".to_string()],
        repo_root: None,
        created_by: Some("codex".to_string()),
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Backlog,
        priority: TaskPriority::High,
        complexity: Some(TaskComplexity::Medium),
        task_type: TaskType::Feature,
        external_refs: Vec::new(),
        source_task_id: None,
        crew: None,
        orchestrator: None,
        comments: Vec::new(),
    }
}

pub(super) fn committed(outcome: TaskCoordinationCommitOutcome) -> TaskCoordinationCommit {
    match outcome {
        TaskCoordinationCommitOutcome::Committed(commit) => commit,
        other => panic!("expected a committed outcome, got {other:?}"),
    }
}
