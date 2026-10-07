#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{ExternalRef, Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::context::RuntimeHost;

use super::super::cleanup::remove_worktree;
use super::super::gc::{WorktreeGcOptions, collect_worktrees};
use super::super::resolve_worktree_path_from_prefix;

struct FakeTaskHost {
    tasks: BTreeMap<String, Task>,
}

impl FakeTaskHost {
    fn new(tasks: Vec<Task>) -> Self {
        Self {
            tasks: tasks
                .into_iter()
                .map(|task| (task.id.clone(), task))
                .collect(),
        }
    }
}

impl RuntimeHost for FakeTaskHost {
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

fn task_fixture(id: &str, status: TaskStatus) -> Task {
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

/// Safety gate: the collector never follows a symlink standing where a
/// worktree should be.
#[cfg(unix)]
#[test]
fn symlink_at_a_known_worktree_path_is_never_followed() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let run = pipeline_run("jrun-symlink", JobRunState::Success, &["ORB-SYMLINK"]);
    let worktree = resolved_task_worktree(&repo, &run);
    let elsewhere = temp.path().join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(elsewhere.join("precious.txt"), "keep me").unwrap();
    fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &worktree).unwrap();
    let host = FakeTaskHost::new(vec![task_fixture("ORB-SYMLINK", TaskStatus::Done)]);

    let result = collect_worktrees(
        &repo,
        &[run],
        &host,
        &WorktreeGcOptions {
            delete: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(elsewhere.join("precious.txt").exists());
    assert_eq!(result.reports[0].action, "skipped:not_a_real_directory");
}

/// The removal itself is the last gate: gc calls `remove_worktree` without
/// `--force`, so a worktree dirtied after the status check makes Git refuse.
/// Never replace this with a recursive delete.
#[test]
fn removal_without_force_fails_closed_on_a_dirty_worktree() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let worktree = repo.join(".orbit/state/worktrees/orbit-jrun-noforce");
    add_worktree(&repo, &worktree, "orbit/noforce");
    fs::write(worktree.join("uncommitted.txt"), "valuable").unwrap();

    let error = remove_worktree(&repo, &worktree, Some("orbit/noforce"), false)
        .expect_err("git must refuse to remove a dirty worktree without --force");

    assert!(worktree.join("uncommitted.txt").exists());
    assert!(
        format!("{error}").contains("worktree remove"),
        "unexpected error: {error}"
    );
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
fn pipeline_run(id: &str, state: JobRunState, task_ids: &[&str]) -> JobRun {
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
fn resolved_task_worktree(repo: &Path, run: &JobRun) -> PathBuf {
    resolve_worktree_path_from_prefix(repo, "orbit", &run.run_id).unwrap()
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", "agent-main"]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
    fs::write(path.join("base.txt"), "base").unwrap();
    git(path, &["add", "base.txt"]);
    git(path, &["commit", "-m", "base"]);
}

fn add_worktree(repo: &Path, path: &Path, branch: &str) {
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
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\n{}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
