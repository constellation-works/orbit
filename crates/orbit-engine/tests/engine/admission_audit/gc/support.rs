#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{ExternalRef, Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

use orbit_engine::run_worktree_paths;

pub(super) struct FakeTaskHost {
    tasks: BTreeMap<String, Task>,
}

impl FakeTaskHost {
    pub(super) fn new(tasks: Vec<Task>) -> Self {
        Self {
            tasks: tasks
                .into_iter()
                .map(|task| (task.id.clone(), task))
                .collect(),
        }
    }
}

impl orbit_engine::test_support::NullRuntimeHost for FakeTaskHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn get_task_artifacts(&self, _task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(Vec::new())
    }

    fn list_tasks_filtered(
        &self,
        _status: Option<TaskStatus>,
        _priority: Option<TaskPriority>,
        _parent_id: Option<&str>,
        _job_run_id: Option<&str>,
        _external_ref: Option<&ExternalRef>,
        _has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        Ok(self.tasks.values().cloned().collect())
    }
}

pub(super) fn task_fixture(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        job_run_machine: None,
        id: id.to_string(),
        title: "fixture task".to_string(),
        description: String::new(),
        acceptance_criteria: Vec::new(),
        tags: Vec::new(),
        required_tools: Vec::new(),
        plan: String::new(),
        execution_summary: String::new(),
        context_files: Vec::new(),
        created_by: None,
        planned_by: None,
        implemented_by: None,
        status,
        priority: TaskPriority::Medium,
        complexity: None,
        task_type: TaskType::Chore,
        pr_status: None,
        external_refs: Vec::new(),
        relations: Vec::new(),
        job_run_id: None,
        crew: None,
        crew_source: None,
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

/// Puts a read-only directory holding a file inside the worktree, the way an
/// aborted fixture leaves one, and keeps it out of the dirty-tree check.
#[cfg(unix)]
pub(super) fn leave_read_only_fixture(repo: &Path, worktree: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    fs::write(repo.join(".git/info/exclude"), ".orbit/tmp/\n").unwrap();
    let locked = worktree.join(".orbit/tmp/x/home/.orbit");
    fs::create_dir_all(&locked).unwrap();
    fs::write(locked.join("state.db"), vec![7u8; 4096]).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    locked
}

/// Lets the tempdir delete the fixture if an assertion fails first.
#[cfg(unix)]
pub(super) struct Unlock(pub(super) PathBuf);

#[cfg(unix)]
impl Drop for Unlock {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

pub(super) fn listed_worktrees(repo: &Path) -> String {
    git(repo, &["worktree", "list", "--porcelain"])
}

/// A run record shaped exactly like a real `task_pr_pipeline` run: `task_ids`
/// as an array, no `branch_prefix`, and no singular `task_id`. Also no
/// `run_id` — the engine injects that into the activity input at dispatch, so
/// the stored `initial_input` never carries it.
///
/// Copied from run `jrun-20260726-0305-2` on dk-server-1 (ORB-10427). GC
/// probed the singular `task_id` against this shape, missed, derived a
/// `parallel-batch-*` path that no worktree ever occupied, and so classified
/// every real worktree `skipped:unrecognized`.
pub(super) fn pipeline_run(id: &str, state: JobRunState, task_ids: &[&str]) -> JobRun {
    job_run(
        id,
        state,
        json!({
            "auto_push": true,
            "base_branch": "agent-main",
            "base_sync": "remote",
            "review": false,
            "task_ids": task_ids,
        }),
    )
}

fn job_run(id: &str, state: JobRunState, input: Value) -> JobRun {
    let now = Utc::now();
    JobRun {
        executed_on: None,
        run_id: id.to_string(),
        job_id: "task_pr_pipeline".to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        duration_ms: Some(1),
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: Some(input),
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    }
}

/// The directory `setup_worktree` creates for a run with no `branch_prefix`
/// override.
pub(super) fn resolved_task_worktree(repo: &Path, run: &JobRun) -> PathBuf {
    run_worktree_paths(repo, run).unwrap().remove(0)
}

pub(super) fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", "agent-main"]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(path.join("base.txt"), "base").unwrap();
    git(path, &["add", "base.txt"]);
    git(path, &["commit", "-m", "base"]);
}

pub(super) fn add_worktree(repo: &Path, path: &Path, branch: &str) {
    git(
        repo,
        &[
            "worktree",
            "add",
            "-b",
            branch,
            path.to_str().unwrap(),
            "HEAD",
        ],
    );
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let mut command = crate::git_test_support::command();
    command.args(args).current_dir(current_dir);
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("spawn {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed ({}):\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
