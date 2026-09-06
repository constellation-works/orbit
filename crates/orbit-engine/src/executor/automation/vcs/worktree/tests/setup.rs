#![allow(missing_docs)]

use std::collections::BTreeMap;
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
use orbit_types::workflow::JobRun;
use tempfile::tempdir;

use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};

use super::super::resolve_worktree_path_from_prefix;
use super::super::setup::{ensure_worktree, setup_worktree, worktree_setup_output};

#[test]
fn ensure_worktree_reattaches_existing_checkout_without_resetting_its_branch() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");

    assert_eq!(
        ensure_worktree(&repo, &worktree, &first_base, "orbit/test").unwrap(),
        "orbit/test"
    );
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), first_base);

    let epic_commit = commit_file(&worktree, "child.txt", "landed");

    let second_base = commit_file(&repo, "base.txt", "v2");
    let reattached = ensure_worktree(&repo, &worktree, &second_base, "orbit/new-name").unwrap();

    assert_eq!(reattached, "orbit/test");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), epic_commit);
}

/// A checkout that is a git work tree but not one this repository registered
/// is refused and left untouched: `clean -fd` must never run on it.
#[test]
fn ensure_worktree_refuses_to_clean_a_foreign_checkout_at_the_resolved_path() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let base = commit_file(&repo, "base.txt", "v1");
    // Someone else's repository lives exactly where this run's worktree would.
    init_repo(&worktree, "main");
    commit_file(&worktree, "theirs.txt", "tracked");
    fs::write(worktree.join("scratch.txt"), "untracked work").unwrap();

    let error = ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not a worktree of this repository"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("scratch.txt")).unwrap(),
        "untracked work"
    );
}

#[test]
fn ensure_worktree_reuses_orphan_branch_from_failed_attempt() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");
    git(&repo, &["branch", "orbit/test", &first_base]);

    let second_base = commit_file(&repo, "base.txt", "v2");
    ensure_worktree(&repo, &worktree, &second_base, "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), first_base);
}

#[test]
fn ensure_worktree_prunes_dangling_metadata_from_failed_attempt() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let base = commit_file(&repo, "base.txt", "v1");

    ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap();
    fs::remove_dir_all(&worktree).unwrap();

    ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), base);
}

#[test]
fn ensure_worktree_reuses_empty_path_from_failed_attempt() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let base = commit_file(&repo, "base.txt", "v1");
    fs::create_dir_all(&worktree).unwrap();

    ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), base);
}

#[test]
fn ensure_worktree_uses_commit_start_point_without_upstream_config() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let local = temp.path().join("local");
    let worktree = temp.path().join("worktree");

    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&seed, "agent-main");
    let remote_head = commit_file(&seed, "base.txt", "v1");
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "agent-main"]);
    git(
        temp.path(),
        &[
            "clone",
            "--branch",
            "agent-main",
            remote.to_str().unwrap(),
            local.to_str().unwrap(),
        ],
    );

    ensure_worktree(&local, &worktree, "origin/agent-main", "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), remote_head);
    assert_git_fails(&local, &["config", "--get", "branch.orbit/test.remote"]);
    assert_git_fails(&local, &["config", "--get", "branch.orbit/test.merge"]);
}

#[test]
fn worktree_setup_output_includes_legacy_batch_id_alias() {
    let output = worktree_setup_output(
        "jrun-test",
        "/tmp/orbit-worktree".to_string(),
        "orbit/ORB-00010".to_string(),
        "main".to_string(),
        "1111111111111111111111111111111111111111".to_string(),
    );

    assert_eq!(output["job_run_id"], json!("jrun-test"));
    assert_eq!(output["batch_id"], output["job_run_id"]);
}

#[test]
fn worktree_setup_publishes_the_resolved_base_commit_alongside_the_moving_ref() {
    // ORB-10380: downstream steps must be able to pin the base this worktree was
    // created at, because `origin/<base>` moves while the run is in flight.
    let output = worktree_setup_output(
        "jrun-test",
        "/tmp/orbit-worktree".to_string(),
        "orbit/ORB-10380".to_string(),
        "origin/agent-main".to_string(),
        "2222222222222222222222222222222222222222".to_string(),
    );

    assert_eq!(output["base_ref"], json!("origin/agent-main"));
    assert_eq!(
        output["base_sha"],
        json!("2222222222222222222222222222222222222222")
    );
}

fn init_repo(path: &Path, branch: &str) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", branch]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
}

fn commit_file(repo: &Path, file_name: &str, contents: &str) -> String {
    fs::write(repo.join(file_name), contents).unwrap();
    git(repo, &["add", file_name]);
    git(repo, &["commit", "-m", &format!("write {file_name}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn assert_git_fails(current_dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "git {} unexpectedly succeeded in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn setup_worktree_refuses_dirty_base_checkout_when_landing_mode_is_local() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    // Introduce dirty landing state in the base repository checkout (simulating init generated files)
    fs::write(repo.join(".gitignore"), ".orbit/*\n").unwrap();
    fs::create_dir_all(repo.join(".orbit").join("auto_tasks")).unwrap();
    fs::write(
        repo.join(".orbit").join("auto_tasks").join("curation.yaml"),
        "enabled: false\n",
    )
    .unwrap();

    let host = FakeHost::new(&repo, &["ORB-11373"]);
    let run_id = "jrun-dirty-local-test";
    let input = json!({
        "task_ids": ["ORB-11373"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "landing_mode": "local",
        "dependency_delivery": "ignore",
    });

    let error = setup_worktree(&host, &input).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("base branch checkout")
            && message.contains("must be clean before merge_batch_worktree_into_base"),
        "expected clean base checkout refusal, got: {message}"
    );

    // Verify refusal prevented worktree creation and task admission
    let worktree_path = resolve_worktree_path_from_prefix(&repo, "orbit", run_id).unwrap();
    assert!(
        !worktree_path.exists(),
        "refused setup must not leave a worktree behind"
    );
    assert!(
        host.admitted().is_empty(),
        "refused setup must not admit the task into workflow"
    );
}

#[test]
fn setup_worktree_allows_dirty_base_checkout_when_landing_mode_is_pr_or_omitted() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    // Introduce dirty state in base checkout
    fs::write(repo.join(".gitignore"), ".orbit/*\n").unwrap();

    let host = FakeHost::new(&repo, &["ORB-11373"]);
    let run_id = "jrun-dirty-pr-test";
    let input = json!({
        "task_ids": ["ORB-11373"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "landing_mode": "pr",
        "dependency_delivery": "ignore",
    });

    let output = setup_worktree(&host, &input).expect("PR mode must allow dirty base checkout");
    assert_eq!(output["job_run_id"], json!(run_id));
    assert_eq!(host.admitted(), vec!["ORB-11373".to_string()]);
}

#[test]
fn setup_worktree_succeeds_when_landing_mode_is_local_and_base_is_clean() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    let host = FakeHost::new(&repo, &["ORB-11373"]);
    let run_id = "jrun-clean-local-test";
    let input = json!({
        "task_ids": ["ORB-11373"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "landing_mode": "local",
        "dependency_delivery": "ignore",
    });

    let output =
        setup_worktree(&host, &input).expect("Clean base checkout must succeed in local mode");
    assert_eq!(output["job_run_id"], json!(run_id));
    assert_eq!(host.admitted(), vec!["ORB-11373".to_string()]);
}

struct FakeHost {
    tasks: BTreeMap<String, Task>,
    repo_root: PathBuf,
    data_root: PathBuf,
    scoreboard_dir: PathBuf,
    admitted: Mutex<Vec<String>>,
}

impl FakeHost {
    fn new(repo_root: &Path, task_ids: &[&str]) -> Self {
        let now = Utc::now();
        let tasks = task_ids
            .iter()
            .map(|id| {
                (
                    id.to_string(),
                    Task {
                        id: id.to_string(),
                        title: format!("Task {id}"),
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
                        status: TaskStatus::Backlog,
                        priority: TaskPriority::Medium,
                        complexity: None,
                        task_type: TaskType::Chore,
                        pr_status: None,
                        external_refs: Vec::new(),
                        relations: Vec::new(),
                        job_run_id: None,
                        crew: None,
                        orchestrator: None,
                        created_at: now,
                        updated_at: now,
                    },
                )
            })
            .collect();
        Self {
            tasks,
            repo_root: repo_root.to_path_buf(),
            data_root: repo_root.join(".orbit-test-data"),
            scoreboard_dir: repo_root.join(".orbit-test-data").join("scoreboard"),
            admitted: Mutex::new(Vec::new()),
        }
    }

    fn admitted(&self) -> Vec<String> {
        self.admitted.lock().expect("admitted lock").clone()
    }
}

impl RuntimeHost for FakeHost {
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

    fn start_task(
        &self,
        _task_id: &str,
        _note: Option<String>,
        _comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "start_task not needed in setup tests".to_string(),
        ))
    }

    fn admit_task_for_workflow(&self, task_id: &str, _workflow: &str) -> Result<Task, OrbitError> {
        self.admitted
            .lock()
            .expect("admitted lock")
            .push(task_id.to_string());
        self.get_task(task_id)
    }

    fn update_task_from_activity(
        &self,
        _task_id: &str,
        _update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "update_task_from_activity not needed in setup tests".to_string(),
        ))
    }

    fn apply_task_automation_update(
        &self,
        _task_id: &str,
        _update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn record_event(&self, _event: OrbitEvent) -> Result<(), OrbitError> {
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo_root.to_string_lossy().to_string())
    }

    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(Vec::new())
    }

    fn run_tool_with_context_and_role(
        &self,
        _name: &str,
        _input: Value,
        _role: Role,
        _tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::Execution(
            "run_tool_with_context_and_role not needed in setup tests".to_string(),
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
