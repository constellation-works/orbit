//! The gated fixture every gate test starts from.
//!
//! Fixtures build a real workspace runtime over a git checkout so admission
//! capture, the gate's Git evidence and ledger budgets exercise the production
//! paths [ORB-11333].

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_types::task::{Task, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::JobRun;
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::application::job::ReviewAdmissionHook;
use crate::application::job::seed_default_jobs;
use crate::application::review::review_gate_admit;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::application::test_support::runtime_with_workspace_config;

/// Workspace config with a before-PR reviewer crew distinct from the
/// implementer; tests append further `[review]` keys.
pub(super) const BEFORE_PR: &str = "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_crew = \"reviewers\"\n[review]\nbefore_pr = true\n";

pub(super) struct Fixture {
    pub(super) _root: TempDir,
    pub(super) runtime: OrbitRuntime,
    pub(super) repo: PathBuf,
}

/// A workspace runtime with the given config over a git checkout whose
/// `main` branch has one commit.
fn fixture(config_toml: &str) -> Fixture {
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
    fs::write(repo.join("src.txt"), "implementation target\n").expect("write source");
    git(&repo, &["add", ".gitignore", "README.md", "src.txt"]);
    git(&repo, &["commit", "-m", "seed"]);
    Fixture {
        _root: root,
        runtime,
        repo: repo.to_path_buf(),
    }
}

pub(super) fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = crate::git_test_support::command()
        .args(args)
        .current_dir(current_dir)
        .env("GIT_AUTHOR_NAME", "orbit[impl-model]")
        .env("GIT_AUTHOR_EMAIL", "agent@orbit.invalid")
        .env("GIT_COMMITTER_NAME", "orbit")
        .env("GIT_COMMITTER_EMAIL", "orbit@orbit.local")
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

/// A task in progress whose scope is the seeded source file.
fn seed_task(runtime: &OrbitRuntime, title: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The fixture outcome is observable.".to_string()],
            plan: "Change the source file.".to_string(),
            context_files: vec!["file:src.txt".to_string()],
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::InProgress),
            ..TaskAddParams::default()
        })
        .expect("seed task")
}

/// Persist a delivery run for `job` carrying the captured review admission,
/// and bind the tasks to it as the pipeline would.
fn admitted_run(runtime: &OrbitRuntime, job: &str, task_ids: &[String]) -> JobRun {
    let mut input = json!({
        "task_ids": task_ids,
        "base_branch": "main",
        "base_sync": "local",
        "allowed_crews": [],
    });
    runtime
        .install_review_admission(job, &mut input, None, false)
        .expect("capture admission");
    let run = RuntimeHost::insert_job_run(runtime, job, 1, Utc::now(), Some(input), None)
        .expect("insert run");
    bind_tasks(runtime, task_ids, &run.run_id);
    run
}

/// Point the tasks at `run_id`, as admitting them to a delivery run does.
fn bind_tasks(runtime: &OrbitRuntime, task_ids: &[String], run_id: &str) {
    for task_id in task_ids {
        runtime
            .update_task(
                task_id,
                TaskUpdateParams {
                    job_run_id: Some(Some(run_id.to_string())),
                    execution_summary: Some(
                        "Outcome: success\n\nChanges:\n- Updated src.txt.".to_string(),
                    ),
                    ..TaskUpdateParams::default()
                },
            )
            .expect("bind task to run");
    }
}

/// Check out a candidate branch with one implementation commit over `main`.
fn implement_candidate(repo: &Path, task_id: &str) -> String {
    git(repo, &["checkout", "-b", &format!("orbit/{task_id}")]);
    fs::write(repo.join("src.txt"), "implementation target\nimplemented\n").expect("edit");
    git(repo, &["add", "src.txt"]);
    git(
        repo,
        &["commit", "-m", &format!("feat: implement [{task_id}]")],
    );
    git(repo, &["rev-parse", "HEAD"])
}

/// Fixture with a task bundle, its admitted run, and a checked-out
/// candidate. `task_id` is the bundle's first task.
pub(super) struct Gated {
    pub(super) fixture: Fixture,
    pub(super) task_id: String,
    pub(super) bundle: Vec<String>,
    pub(super) run_id: String,
}

pub(super) fn gated_fixture(config: &str) -> Gated {
    gated_bundle_fixture(config, 1)
}

/// A gated fixture delivering `tasks` tasks together in one run.
pub(super) fn gated_bundle_fixture(config: &str, tasks: usize) -> Gated {
    let fixture = fixture(config);
    let bundle = (0..tasks)
        .map(|index| seed_task(&fixture.runtime, &format!("gated change {index}")).id)
        .collect::<Vec<_>>();
    let run = admitted_run(&fixture.runtime, "task_pr_pipeline", &bundle);
    let _implementation_sha = implement_candidate(&fixture.repo, &bundle[0]);
    Gated {
        fixture,
        task_id: bundle[0].clone(),
        bundle,
        run_id: run.run_id,
    }
}

impl Gated {
    /// Rewrite the workspace config and reopen the runtime over it, as an
    /// operator editing `config.toml` mid-run does: runs already submitted
    /// keep the admission they captured.
    pub(super) fn reconfigure(&mut self, config_toml: &str) {
        let workspace = self.fixture.repo.join(".orbit");
        fs::write(workspace.join("config.toml"), config_toml).expect("rewrite config");
        self.fixture.runtime =
            OrbitRuntime::from_roots(&self.fixture.runtime.global_root(), &workspace)
                .expect("reopen runtime");
    }

    /// A fresh delivery run of the same bundle, as re-admission dispatches.
    pub(super) fn fresh_run(&self) -> String {
        let run = admitted_run(&self.fixture.runtime, "task_pr_pipeline", &self.bundle);
        self.start(&run.run_id, Utc::now());
        run.run_id
    }

    /// Record `run_id` as running since `started_at`.
    pub(super) fn start(&self, run_id: &str, started_at: chrono::DateTime<Utc>) {
        RuntimeHost::mark_job_run_running(
            &self.fixture.runtime,
            run_id,
            started_at,
            std::process::id(),
        )
        .expect("mark run running");
    }

    pub(super) fn admit(&self) -> Result<Value, orbit_engine::DispatchError> {
        self.admit_in(&self.run_id)
    }

    pub(super) fn admit_in(&self, run_id: &str) -> Result<Value, orbit_engine::DispatchError> {
        review_gate_admit(
            &self.fixture.runtime,
            "review_gate_admit",
            &json!({
                "job_run_id": run_id,
                "completed_task_ids": self.bundle,
                "workspace_path": self.fixture.repo,
                "base": "main",
                "base_sync": "local",
                "mode": "pr",
                "skipped_no_diff_expected": false,
                "allowed_crews": [],
            }),
        )
    }
}
