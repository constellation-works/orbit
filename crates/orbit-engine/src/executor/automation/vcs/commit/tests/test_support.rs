use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    CONTEXT_FILES_WIDENED_EVENT, ContextFilesWidening, ContextWideningStep, ExecutionLocation,
    Task, TaskHistoryEntry, TaskPriority, TaskStatus, TaskType,
};
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::JobRun;
use tempfile::tempdir;

use crate::context::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};

use super::super::super::git::git_success;

pub struct CommitTestHost {
    tasks: Mutex<Vec<Task>>,
    repo_root: PathBuf,
    data_root: PathBuf,
    scoreboard_dir: PathBuf,
    /// The trusted claim binding a claimed leaf's host carries, if any.
    worker: Option<WorkerInvocation>,
    /// Task history, including the selector widenings this host recorded.
    history: Mutex<Vec<(String, TaskHistoryEntry)>>,
}

impl CommitTestHost {
    pub fn new(tasks: Vec<Task>, repo_root: PathBuf) -> Self {
        let data_root = repo_root.join(".orbit-test-data");
        let scoreboard_dir = data_root.join("scoreboard");
        Self {
            tasks: Mutex::new(tasks),
            repo_root,
            data_root,
            scoreboard_dir,
            worker: None,
            history: Mutex::new(Vec::new()),
        }
    }

    /// Every widening recorded for `task_id`, in order.
    pub fn widenings(&self, task_id: &str) -> Vec<ContextFilesWidening> {
        self.get_task_history(task_id)
            .unwrap()
            .iter()
            .filter(|entry| entry.event == CONTEXT_FILES_WIDENED_EVENT)
            .filter_map(|entry| {
                entry
                    .note
                    .as_deref()
                    .and_then(ContextFilesWidening::from_note)
            })
            .collect()
    }

    /// Record that `task_id`'s agent changed `paths`, as the boundary guard
    /// does when an implementer exits.
    pub fn with_agent_widening(self, task_id: &str, paths: &[&str]) -> Self {
        let paths = paths
            .iter()
            .map(|path| path.to_string())
            .collect::<Vec<_>>();
        self.widen_task_context_files(
            task_id,
            "batch-1",
            ContextWideningStep::Implement,
            "agent_implement",
            &paths,
        )
        .unwrap();
        self
    }

    pub fn task(&self, task_id: &str) -> Task {
        self.get_task(task_id).unwrap()
    }

    /// Run as a claimed leaf bound to `task_id`, as a follower's worker is.
    pub fn with_claim_binding(mut self, task_id: &str) -> Self {
        self.worker = Some(WorkerInvocation {
            owner_machine_id: "owner-machine".into(),
            owner_workspace_id: "owner-workspace".into(),
            owner_destination: "owner-machine/owner-workspace".into(),
            task_id: task_id.into(),
            claim_id: "claim-1".into(),
            execution: ExecutionLocation {
                machine_id: "follower-machine".into(),
                machine_name: None,
            },
            bound_run_id: "batch-1".into(),
        });
        self
    }
}

impl RuntimeHost for CommitTestHost {
    fn get_job_run(&self, _run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        Ok(None)
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .lock()
            .unwrap()
            .iter()
            .find(|task| task.id == task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        batch_id: Option<&str>,
        external_ref: Option<&orbit_types::task::ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        Ok(self
            .tasks
            .lock()
            .unwrap()
            .iter()
            .filter(|task| status.is_none_or(|status| task.status == status))
            .filter(|task| priority.is_none_or(|priority| task.priority == priority))
            .filter(|task| parent_id.is_none_or(|parent_id| task.parent_id() == Some(parent_id)))
            .filter(|task| {
                batch_id.is_none_or(|batch_id| task.job_run_id.as_deref() == Some(batch_id))
            })
            .filter(|task| {
                external_ref.is_none_or(|external_ref| {
                    task.external_refs.iter().any(|candidate| {
                        candidate.system == external_ref.system && candidate.id == external_ref.id
                    })
                })
            })
            .filter(|task| {
                has_external_ref_system.is_none_or(|system| {
                    task.external_refs
                        .iter()
                        .any(|candidate| candidate.system == system)
                })
            })
            .cloned()
            .collect())
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks.iter_mut().find(|task| task.id == task_id).unwrap();
        assert_eq!(task.status, update.expected_status);
        task.status = update.status;
        Ok(task.clone())
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        if let Some(status) = update.status {
            task.status = status;
        }
        if let Some(execution_summary) = update.execution_summary {
            task.execution_summary = execution_summary;
        }
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo_root.to_string_lossy().to_string())
    }

    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn scoreboard_dir(&self) -> &Path {
        &self.scoreboard_dir
    }

    fn worker_invocation(&self) -> Option<WorkerInvocation> {
        self.worker.clone()
    }

    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        Ok(self
            .history
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| id == task_id)
            .map(|(_, entry)| entry.clone())
            .collect())
    }

    /// Mirrors the owner host: exact `file:` selectors for uncovered paths
    /// plus one provenance entry; a claimed leaf widens nothing.
    fn widen_task_context_files(
        &self,
        task_id: &str,
        run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        if self.worker.is_some() {
            return Ok(Vec::new());
        }
        let mut tasks = self.tasks.lock().unwrap();
        let task = tasks
            .iter_mut()
            .find(|task| task.id == task_id)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))?;
        let added = paths
            .iter()
            .map(|path| format!("file:{path}"))
            .filter(|selector| {
                !task
                    .context_files
                    .iter()
                    .any(|existing| orbit_common::fs::selector::overlaps(existing, selector))
            })
            .collect::<Vec<_>>();
        if added.is_empty() {
            return Ok(added);
        }
        task.context_files.extend(added.iter().cloned());
        let note = serde_json::to_string(&ContextFilesWidening {
            run_id: run_id.to_string(),
            step,
            activity: activity.to_string(),
            selectors: added.clone(),
        })
        .unwrap();
        self.history.lock().unwrap().push((
            task_id.to_string(),
            TaskHistoryEntry {
                at: Utc::now(),
                by: "system".to_string(),
                event: CONTEXT_FILES_WIDENED_EVENT.to_string(),
                note: Some(note),
                from_status: None,
                to_status: None,
            },
        ));
        Ok(added)
    }
}

/// Detach a fixture repo from any machine-global `core.hooksPath`.
///
/// A developer box may configure arbitrary global Git hooks. A fixture repo
/// must not inherit host-specific commit mutation, so its empty hooks directory
/// makes the no-hook production contract explicit and deterministic.
fn detach_global_git_hooks(repo: &Path) {
    let hooks = repo.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).expect("create empty hooks dir");
    git_success(
        repo,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    )
    .expect("config core.hooksPath");
}

pub fn initialized_git_repo() -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    git_success(repo, &["init"]).expect("git init");
    detach_global_git_hooks(repo);
    git_success(repo, &["config", "user.name", "Local User"]).expect("config user.name");
    git_success(repo, &["config", "user.email", "local@example.test"]).expect("config user.email");
    fs::write(repo.join("README.md"), "base\n").unwrap();
    git_success(repo, &["add", "README.md"]).expect("git add");
    git_success(repo, &["commit", "-m", "initial commit"]).expect("initial commit");
    temp
}

pub fn task_with_file(id: &str, title: &str, path: &str, implemented_by: &str) -> Task {
    let now = Utc::now();
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: title.to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        // ORB-10313: the delivery gate reads the durable outcome before touching
        // the checkout.
        execution_summary: "Outcome: success".to_string(),
        context_files: vec![format!("file:{path}")],
        created_by: None,
        planned_by: None,
        implemented_by: Some(implemented_by.to_string()),
        status: TaskStatus::InProgress,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: Some("batch-1".to_string()),
        crew: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}
