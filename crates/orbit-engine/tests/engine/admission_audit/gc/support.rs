#![allow(missing_docs)]

use orbit_engine::test_support::impl_test_host;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
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

impl_test_host! {
    impl HostEnvironment for FakeTaskHost {
        fn lookup_task_for_worktree_gc(
            &self,
            _run_id: &str,
            task_id: &str,
        ) -> orbit_engine::WorktreeGcTaskLookup {
            use orbit_engine::WorktreeGcTaskLookup;

            match self.tasks.get(task_id) {
                Some(task) => WorktreeGcTaskLookup::Found {
                    status: task.status,
                    pr_status: task.pr_status.clone(),
                    status_since: Some(task.updated_at),
                },
                None => WorktreeGcTaskLookup::Unresolved,
            }
        }
    }
}

impl_test_host! {
    impl RuntimeHostProcessEnvironment for FakeTaskHost {}
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

pub(super) fn task_fixture(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        granted_tools: Vec::new(),
        requires_labels: Vec::new(),
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
        orchestrator_model: None,
        orchestrator_model_source: None,
        created_by_model: None,
        origin: None,
        rule_fingerprint: None,
        created_at: now,
        updated_at: now,
        context_creation_sealed_at: None,
        not_before: None,
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
        build: None,
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
    run_worktree_paths(&orbit_common::Env::capture(), repo, run)
        .unwrap()
        .remove(0)
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
