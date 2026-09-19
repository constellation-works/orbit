#![allow(missing_docs)]

//! Every fixture here is a plain local repository: an unmerged dependency
//! branch, a remote-tracking ref written with `update-ref`, or a merge into the
//! base. None of them consults GitHub, so a PR-backed dependency and a
//! locally-committed one are exercised by the same deterministic Git state
//! (ORB-10464).

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
use orbit_types::task::{
    ExternalRef, Task, TaskArtifact, TaskPriority, TaskRelation, TaskRelationType, TaskStatus,
    TaskType,
};
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::context::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};

use super::super::dependency_delivery::{
    DependencyDeliveryMode, dependency_delivery_mode_from_input,
    ensure_dependencies_delivered_into_base,
};
use super::super::resolve_worktree_path_from_prefix;
use super::super::setup::setup_worktree;

const BASE_BRANCH: &str = "agent-main";
const RUN_ID: &str = "jrun-dependency-delivery";

#[test]
fn setup_refuses_before_creating_a_worktree_when_a_done_dependency_is_unmerged() {
    // F2026-07-038 verbatim: the dependency is done and its branch exists, but
    // the base the worktree would be cut from does not contain its commit.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    commit_marked(&repo, "ORB-BASE", "base.txt", "v1");
    let dependency_sha = commit_on_branch(&repo, "orbit/ORB-DEP", "ORB-DEP", "dep.txt", "fixed");

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Done),
        ],
    );
    let worktree_path = resolve_worktree_path_from_prefix(&repo, "orbit", RUN_ID).unwrap();

    let error = setup_worktree(&host, &setup_input()).unwrap_err();

    let OrbitError::DependencyNotDelivered(diagnostic) = &error else {
        panic!("expected DependencyNotDelivered, got {error:?}");
    };
    assert_eq!(diagnostic.task_id, "ORB-TASK");
    assert_eq!(diagnostic.dependency_id, "ORB-DEP");
    assert_eq!(diagnostic.base_sha, git(&repo, &["rev-parse", BASE_BRANCH]));
    assert!(
        diagnostic.detail.contains(&dependency_sha),
        "diagnostic must name the missing delivery commit: {}",
        diagnostic.detail
    );
    assert!(
        !worktree_path.exists(),
        "refusal must not leave a worktree behind"
    );
    assert!(
        host.admitted().is_empty(),
        "refusal must not admit the task into the workflow"
    );
}

#[test]
fn setup_admits_when_the_done_dependency_is_merged_into_the_base() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    commit_marked(&repo, "ORB-BASE", "base.txt", "v1");
    commit_on_branch(&repo, "orbit/ORB-DEP", "ORB-DEP", "dep.txt", "fixed");
    git(
        &repo,
        &["merge", "--no-ff", "-m", "merge dep", "orbit/ORB-DEP"],
    );

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Done),
        ],
    );
    let worktree_path = resolve_worktree_path_from_prefix(&repo, "orbit", RUN_ID).unwrap();

    let output = setup_worktree(&host, &setup_input()).unwrap();

    assert_eq!(
        output["base_sha"],
        json!(git(&repo, &["rev-parse", "HEAD"]))
    );
    assert!(worktree_path.exists());
    assert_eq!(host.admitted(), vec!["ORB-TASK".to_string()]);
}

#[test]
fn setup_skips_the_gate_when_delivery_enforcement_is_turned_off() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    commit_marked(&repo, "ORB-BASE", "base.txt", "v1");
    commit_on_branch(&repo, "orbit/ORB-DEP", "ORB-DEP", "dep.txt", "fixed");

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Done),
        ],
    );
    let mut input = setup_input();
    input["dependency_delivery"] = json!("ignore");

    setup_worktree(&host, &input).unwrap();

    assert_eq!(host.admitted(), vec!["ORB-TASK".to_string()]);
}

#[test]
fn a_pr_backed_dependency_pushed_but_unmerged_blocks() {
    // The PR-backed shape without any GitHub state: the branch exists only as
    // a remote-tracking ref, exactly as it does after a push with the PR still
    // open.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let base_sha = commit_marked(&repo, "ORB-BASE", "base.txt", "v1");
    let dependency_sha = commit_on_branch(&repo, "pushed", "ORB-DEP", "dep.txt", "fixed");
    git(
        &repo,
        &[
            "update-ref",
            "refs/remotes/origin/orbit/ORB-DEP",
            &dependency_sha,
        ],
    );
    git(&repo, &["branch", "-D", "pushed"]);

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Done),
        ],
    );

    let error = ensure_dependencies_delivered_into_base(
        &host,
        &repo,
        &["ORB-TASK".to_string()],
        "origin/agent-main",
        &base_sha,
    )
    .unwrap_err();

    assert!(
        matches!(&error, OrbitError::DependencyNotDelivered(diagnostic) if diagnostic.dependency_id == "ORB-DEP"),
        "expected the pushed-but-unmerged dependency to block, got {error:?}"
    );
}

#[test]
fn a_dependency_squash_merged_under_a_new_sha_is_delivered() {
    // A squash merge rewrites the sha but carries the task marker, so delivery
    // is decided by the marker reaching the base, not by the original commit.
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    commit_marked(&repo, "ORB-BASE", "base.txt", "v1");
    let original = commit_on_branch(&repo, "orbit/ORB-DEP", "ORB-DEP", "dep.txt", "fixed");
    git(&repo, &["merge", "--squash", "orbit/ORB-DEP"]);
    git(&repo, &["commit", "-m", "[ORB-DEP] squashed delivery"]);
    let base_sha = git(&repo, &["rev-parse", "HEAD"]);
    assert_ne!(base_sha, original);

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Done),
        ],
    );

    ensure_dependencies_delivered_into_base(
        &host,
        &repo,
        &["ORB-TASK".to_string()],
        BASE_BRANCH,
        &base_sha,
    )
    .unwrap();
}

#[test]
fn a_done_dependency_with_no_commit_in_the_repository_is_left_alone() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let base_sha = commit_marked(&repo, "ORB-BASE", "base.txt", "v1");

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Done),
        ],
    );

    ensure_dependencies_delivered_into_base(
        &host,
        &repo,
        &["ORB-TASK".to_string()],
        BASE_BRANCH,
        &base_sha,
    )
    .unwrap();
}

#[test]
fn an_unfinished_dependency_stays_with_the_lifecycle_gate() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let base_sha = commit_marked(&repo, "ORB-BASE", "base.txt", "v1");
    commit_on_branch(&repo, "orbit/ORB-DEP", "ORB-DEP", "dep.txt", "wip");

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Review),
        ],
    );

    ensure_dependencies_delivered_into_base(
        &host,
        &repo,
        &["ORB-TASK".to_string()],
        BASE_BRANCH,
        &base_sha,
    )
    .unwrap();
}

#[test]
fn a_dependency_shipping_in_the_same_run_delivers_itself() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let base_sha = commit_marked(&repo, "ORB-BASE", "base.txt", "v1");
    commit_on_branch(&repo, "orbit/ORB-DEP", "ORB-DEP", "dep.txt", "fixed");

    let host = FakeHost::new(
        &repo,
        vec![
            dependent_task("ORB-TASK", "ORB-DEP"),
            task_fixture("ORB-DEP", TaskStatus::Done),
        ],
    );

    ensure_dependencies_delivered_into_base(
        &host,
        &repo,
        &["ORB-TASK".to_string(), "ORB-DEP".to_string()],
        BASE_BRANCH,
        &base_sha,
    )
    .unwrap();
}

#[test]
fn missing_dependency_tasks_do_not_reach_the_delivery_check() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo);
    let base_sha = commit_marked(&repo, "ORB-BASE", "base.txt", "v1");

    let host = FakeHost::new(&repo, vec![dependent_task("ORB-TASK", "ORB-GONE")]);

    ensure_dependencies_delivered_into_base(
        &host,
        &repo,
        &["ORB-TASK".to_string()],
        BASE_BRANCH,
        &base_sha,
    )
    .unwrap();
}

#[test]
fn delivery_mode_defaults_to_enforce_and_rejects_unknown_values() {
    assert_eq!(
        dependency_delivery_mode_from_input(&json!({})).unwrap(),
        DependencyDeliveryMode::Enforce
    );
    assert_eq!(
        dependency_delivery_mode_from_input(&json!({"dependency_delivery": "ignore"})).unwrap(),
        DependencyDeliveryMode::Ignore
    );
    let error =
        dependency_delivery_mode_from_input(&json!({"dependency_delivery": "maybe"})).unwrap_err();
    assert!(
        error.to_string().contains("dependency_delivery"),
        "unexpected error: {error}"
    );
}

fn setup_input() -> Value {
    json!({
        "task_ids": ["ORB-TASK"],
        "run_id": RUN_ID,
        "base": BASE_BRANCH,
        "base_sync": "local",
    })
}

struct FakeHost {
    tasks: BTreeMap<String, Task>,
    repo_root: PathBuf,
    data_root: PathBuf,
    scoreboard_dir: PathBuf,
    admitted: Mutex<Vec<String>>,
}

impl FakeHost {
    fn new(repo_root: &Path, tasks: Vec<Task>) -> Self {
        Self {
            tasks: tasks
                .into_iter()
                .map(|task| (task.id.clone(), task))
                .collect(),
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
            "start_task is not needed by dependency delivery tests".to_string(),
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
            "update_task_from_activity is not needed by dependency delivery tests".to_string(),
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
            "run_tool_with_context_and_role is not needed by dependency delivery tests".to_string(),
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

fn dependent_task(id: &str, dependency_id: &str) -> Task {
    let mut task = task_fixture(id, TaskStatus::Backlog);
    task.relations = vec![TaskRelation {
        relation_type: TaskRelationType::BlockedBy,
        target: dependency_id.to_string(),
    }];
    task
}

fn task_fixture(id: &str, status: TaskStatus) -> Task {
    let now = Utc::now();
    Task {
        job_run_host: None,
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
        orchestrator: None,
        created_at: now,
        updated_at: now,
    }
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", BASE_BRANCH]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
    // A machine-global `core.hooksPath` rewrites fixture commit messages
    // (ORB-10350); the marker these tests grep for must be exactly what the
    // test wrote.
    let hooks = path.join(".git").join("orbit-test-empty-hooks");
    fs::create_dir_all(&hooks).unwrap();
    git(
        path,
        &["config", "core.hooksPath", &hooks.to_string_lossy()],
    );
}

fn commit_marked(repo: &Path, task_id: &str, file_name: &str, contents: &str) -> String {
    fs::write(repo.join(file_name), contents).unwrap();
    git(repo, &["add", file_name]);
    git(
        repo,
        &["commit", "-m", &format!("[{task_id}] write {file_name}")],
    );
    git(repo, &["rev-parse", "HEAD"])
}

/// Commit `task_id`'s work on its own branch and return to the base, leaving
/// the branch unmerged.
fn commit_on_branch(
    repo: &Path,
    branch: &str,
    task_id: &str,
    file_name: &str,
    contents: &str,
) -> String {
    git(repo, &["checkout", "-b", branch]);
    let sha = commit_marked(repo, task_id, file_name, contents);
    git(repo, &["checkout", BASE_BRANCH]);
    sha
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
