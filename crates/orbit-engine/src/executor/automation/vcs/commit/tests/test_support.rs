use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{ExternalRef, Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::Value;
use tempfile::tempdir;

use crate::context::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};

use super::super::super::git::git_success;

pub struct CommitTestHost {
    tasks: Mutex<Vec<Task>>,
    /// ORB-10603: every `execution_summary` an automation update persisted, in
    /// call order, so a test can assert what durable state actually received.
    persisted_summaries: Mutex<Vec<(String, String)>>,
    crew_model: Option<String>,
    repo_root: PathBuf,
    data_root: PathBuf,
    scoreboard_dir: PathBuf,
    job_runs: Mutex<Vec<JobRun>>,
    run_states: Mutex<HashMap<String, PipelineState>>,
    artifacts: Vec<TaskArtifact>,
}

impl CommitTestHost {
    pub fn with_artifacts(mut self, artifacts: Vec<TaskArtifact>) -> Self {
        self.artifacts = artifacts;
        self
    }

    pub fn new(tasks: Vec<Task>, repo_root: PathBuf) -> Self {
        let data_root = repo_root.join(".orbit-test-data");
        let scoreboard_dir = data_root.join("scoreboard");
        Self {
            tasks: Mutex::new(tasks),
            persisted_summaries: Mutex::new(Vec::new()),
            crew_model: None,
            repo_root,
            data_root,
            scoreboard_dir,
            job_runs: Mutex::new(Vec::new()),
            run_states: Mutex::new(HashMap::new()),
            artifacts: Vec::new(),
        }
    }

    pub fn with_crew_model(mut self, model: impl Into<String>) -> Self {
        self.crew_model = Some(model.into());
        self
    }

    pub fn persisted_summaries(&self) -> Vec<(String, String)> {
        self.persisted_summaries.lock().unwrap().clone()
    }

    pub fn with_run_state(
        self,
        run_id: &str,
        retry_source_run_id: Option<&str>,
        state: PipelineState,
    ) -> Self {
        let now = Utc::now();
        self.job_runs.lock().unwrap().push(JobRun {
            run_id: run_id.to_string(),
            job_id: state.job_id.clone(),
            attempt: 1,
            state: JobRunState::Failed,
            scheduled_at: now,
            started_at: Some(now),
            finished_at: Some(now),
            duration_ms: Some(1),
            created_at: now,
            pid: None,
            pid_start_time: None,
            input: Some(state.initial_input.clone()),
            retry_source_run_id: retry_source_run_id.map(ToOwned::to_owned),
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        });
        self.run_states
            .lock()
            .unwrap()
            .insert(run_id.to_string(), state);
        self
    }

    pub fn task_execution_summary(&self, task_id: &str) -> String {
        self.tasks
            .lock()
            .unwrap()
            .iter()
            .find(|task| task.id == task_id)
            .map(|task| task.execution_summary.clone())
            .expect("task exists in the commit test host")
    }
}

impl RuntimeHost for CommitTestHost {
    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        Ok(self
            .job_runs
            .lock()
            .unwrap()
            .iter()
            .find(|run| run.run_id == run_id)
            .cloned())
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        Ok(self.run_states.lock().unwrap().get(run_id).cloned())
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

    fn get_task_artifacts(&self, _task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(self.artifacts.clone())
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

    fn start_task(
        &self,
        _task_id: &str,
        _note: Option<String>,
        _comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "start_task is not needed by commit tests".to_string(),
        ))
    }

    fn admit_task_for_workflow(&self, _task_id: &str, _workflow: &str) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "admit_task_for_workflow is not needed by commit tests".to_string(),
        ))
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
            task.execution_summary = execution_summary.clone();
            self.persisted_summaries
                .lock()
                .unwrap()
                .push((task_id.to_string(), execution_summary));
        }
        Ok(())
    }

    fn record_event(&self, _event: OrbitEvent) -> Result<(), OrbitError> {
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo_root.to_string_lossy().to_string())
    }

    fn resolved_crew_model(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(self.crew_model.clone())
    }

    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn run_tool_with_context_and_role(
        &self,
        _name: &str,
        _input: Value,
        _role: Role,
        _tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::Execution(
            "run_tool_with_context_and_role is not needed by commit tests".to_string(),
        ))
    }

    fn maybe_create_failure_task(
        &self,
        _job_id: &str,
        _run_id: &str,
        _error_code: &str,
        _error_message: &str,
        _agent: Option<&str>,
        _model: Option<&str>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn scoring_enabled(&self) -> bool {
        false
    }

    fn scoreboard_dir(&self) -> &Path {
        &self.scoreboard_dir
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

pub fn initialized_git_repo_without_local_user_config() -> tempfile::TempDir {
    let temp = tempdir().unwrap();
    let repo = temp.path();
    git_success(repo, &["init"]).expect("git init");
    detach_global_git_hooks(repo);
    fs::write(repo.join("README.md"), "base\n").unwrap();
    git_success(repo, &["add", "README.md"]).expect("git add");
    git_success(
        repo,
        &[
            "-c",
            "user.name=Initial User",
            "-c",
            "user.email=initial@example.test",
            "commit",
            "-m",
            "initial commit",
        ],
    )
    .expect("initial commit");
    assert_eq!(
        local_user_config_snapshot(repo),
        CommandSnapshot {
            code: Some(1),
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    );
    temp
}

#[derive(Debug, Eq, PartialEq)]
pub struct CommandSnapshot {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn local_user_config_snapshot(repo: &Path) -> CommandSnapshot {
    git_command_snapshot(repo, &["config", "--local", "--get-regexp", "^user\\."])
}

pub fn git_stdout_bytes(repo: &Path, args: &[&str], context: &str) -> Vec<u8> {
    let snapshot = git_command_snapshot(repo, args);
    assert_eq!(
        snapshot.code,
        Some(0),
        "{context}: stderr={}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    snapshot.stdout
}

fn git_command_snapshot(repo: &Path, args: &[&str]) -> CommandSnapshot {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("run git command");
    CommandSnapshot {
        code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

pub fn task_with_file(id: &str, title: &str, path: &str, implemented_by: &str) -> Task {
    let now = Utc::now();
    Task {
        id: id.to_string(),
        title: title.to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        // ORB-10313: the delivery gate reads the durable outcome before touching
        // the checkout. Individual tests override this meaningful default to
        // exercise failed and nonstandard outcomes.
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

pub fn external_ref(system: &str, id: &str) -> ExternalRef {
    ExternalRef::try_new(system.to_string(), id.to_string(), None)
        .expect("external ref fixture is valid")
}
