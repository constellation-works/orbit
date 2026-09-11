//! Operation-mode integration tests [ORB-11332].
//!
//! Fixtures build a real workspace runtime over a git checkout so grant
//! enablement, the drain classifier, child admission, promotion evidence,
//! and recovery budgets exercise the same paths production uses.

mod grant;
mod pipeline;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_config;
use crate::application::job::pipeline::worker_command_override;
use crate::application::job::seed_default_jobs;
use crate::application::task::TaskAddParams;

/// Registered machine identity every fixture runtime carries.
pub(super) const MACHINE: &str = "hm_test";

pub(super) struct Fixture {
    pub(super) _root: TempDir,
    pub(super) runtime: OrbitRuntime,
    pub(super) repo: PathBuf,
}

/// A workspace runtime with the given `[operation]` config, over a git
/// checkout whose `main` branch has one commit.
pub(super) fn fixture(config_toml: &str) -> Fixture {
    let (root, runtime, repo) = runtime_with_workspace_config(Some(config_toml));
    seed_default_jobs(&runtime.global_root().join("resources/jobs"), true)
        .expect("seed the shipped job catalog");
    git(&repo, &["init"]);
    git(&repo, &["checkout", "-b", "main"]);
    git(&repo, &["config", "user.name", "Orbit Test"]);
    git(&repo, &["config", "user.email", "orbit-test@example.com"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    fs::write(repo.join(".gitignore"), ".orbit/\n").expect("ignore orbit store");
    fs::write(repo.join("README.md"), "fixture\n").expect("write readme");
    git(&repo, &["add", ".gitignore", "README.md"]);
    git(&repo, &["commit", "-m", "seed"]);
    Fixture {
        _root: root,
        runtime: runtime.with_automation_machine_identity(Some(MACHINE.to_string())),
        repo: repo.to_path_buf(),
    }
}

pub(super) fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(super) fn seed_task(runtime: &OrbitRuntime, title: &str, status: TaskStatus) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The fixture outcome is observable.".to_string()],
            plan: "Inspect and update the fixture.".to_string(),
            context_files: vec!["README.md".to_string()],
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(status),
            ..TaskAddParams::default()
        })
        .expect("seed task")
}

/// Replaces the detached pipeline worker with a short-lived shell for the
/// test's thread, so submissions persist runs without re-executing the test
/// binary. The stub outlives the test so lazy orphan reconciliation does not
/// interrupt the pending coordinator while children are admitted under it.
pub(super) struct WorkerOverride;

impl WorkerOverride {
    pub(super) fn install() -> Self {
        worker_command_override::set(["sh", "-c", "sleep 5"]);
        Self
    }
}

impl Drop for WorkerOverride {
    fn drop(&mut self) {
        worker_command_override::clear();
    }
}
