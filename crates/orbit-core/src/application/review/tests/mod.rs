//! Review policy tests [ORB-11333].
//!
//! Fixtures build a real workspace runtime over a git checkout so admission
//! capture, the gate's Git evidence, reviewer-attributed repair commits,
//! ledger budgets, and coverage exclusions exercise the production paths.

mod admission;
mod gate;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use orbit_engine::RuntimeHost;
use orbit_types::task::{Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{
    FindingDisposition, JobRun, REVIEW_CONTRACT_VERSION, REVIEW_REPORT_ARTIFACT, ReviewFinding,
    ReviewReport, ReviewValidation, ReviewVerdict, ValidationOutcome, ValidationRole,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_config;
use crate::application::job::seed_default_jobs;
use crate::application::review::install_review_admission;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

pub(super) const GATED_CONFIG: &str = r#"
[crews.implementer]
model = "impl-model"
provider = "codex"
backend = "cli"

[crews.reviewers]
model = "review-model"
provider = "codex"
backend = "cli"

[workflow]
default_crew = "implementer"

[operation]
review_policy = "before-pr"
review_crew = "reviewers"
"#;

pub(super) struct Fixture {
    pub(super) _root: TempDir,
    pub(super) runtime: OrbitRuntime,
    pub(super) repo: PathBuf,
}

/// A workspace runtime with the given config over a git checkout whose
/// `main` branch has one commit.
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
    let output = Command::new("git")
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
pub(super) fn seed_task(runtime: &OrbitRuntime, title: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}"),
            acceptance_criteria: vec!["The fixture outcome is observable.".to_string()],
            plan: "Change the source file.".to_string(),
            context_files: vec!["file:src.txt".to_string()],
            workspace_path: Some(".".to_string()),
            priority: TaskPriority::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::InProgress),
            ..TaskAddParams::default()
        })
        .expect("seed task")
}

/// Persist a delivery run for `job` carrying the captured review admission,
/// and bind the tasks to it as the pipeline would.
pub(super) fn admitted_run(runtime: &OrbitRuntime, job: &str, task_ids: &[String]) -> JobRun {
    let mut input = json!({
        "task_ids": task_ids,
        "base_branch": "main",
        "base_sync": "local",
        "allowed_crews": [],
    });
    install_review_admission(runtime, job, &mut input, None, false).expect("capture admission");
    let run = RuntimeHost::insert_job_run(runtime, job, 1, Utc::now(), Some(input), None)
        .expect("insert run");
    for task_id in task_ids {
        runtime
            .update_task(
                task_id,
                TaskUpdateParams {
                    job_run_id: Some(Some(run.run_id.clone())),
                    execution_summary: Some(
                        "Outcome: success\n\nChanges:\n- Updated src.txt.".to_string(),
                    ),
                    ..TaskUpdateParams::default()
                },
            )
            .expect("bind task to run");
    }
    run
}

/// Check out a candidate branch with one implementation commit over `main`.
pub(super) fn implement_candidate(repo: &Path, task_id: &str) -> String {
    git(repo, &["checkout", "-b", &format!("orbit/{task_id}")]);
    fs::write(repo.join("src.txt"), "implementation target\nimplemented\n").expect("edit");
    git(repo, &["add", "src.txt"]);
    git(
        repo,
        &["commit", "-m", &format!("feat: implement [{task_id}]")],
    );
    git(repo, &["rev-parse", "HEAD"])
}

pub(super) fn admit_input(run_id: &str, task_ids: &[String], repo: &Path) -> Value {
    json!({
        "job_run_id": run_id,
        "completed_task_ids": task_ids,
        "workspace_path": repo,
        "base": "main",
        "base_sync": "local",
        "mode": "pr",
        "skipped_no_diff_expected": false,
        "allowed_crews": [],
    })
}

pub(super) fn settle_input(
    run_id: &str,
    task_ids: &[String],
    repo: &Path,
    admission: &Value,
) -> Value {
    json!({
        "job_run_id": run_id,
        "completed_task_ids": task_ids,
        "workspace_path": repo,
        "base": "main",
        "base_sync": "local",
        "admission": admission,
    })
}

pub(super) fn report(attempt_id: &str, verdict: ReviewVerdict, repaired: bool) -> ReviewReport {
    ReviewReport {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.to_string(),
        verdict,
        summary: "Checked the change against the criteria.".to_string(),
        findings: if repaired || verdict == ReviewVerdict::ChangesRequired {
            vec![ReviewFinding {
                id: "F1".to_string(),
                severity: "medium".to_string(),
                summary: "Missing trailing note".to_string(),
                paths: vec!["src.txt".to_string()],
                disposition: if repaired {
                    FindingDisposition::Repaired
                } else {
                    FindingDisposition::Open
                },
            }]
        } else {
            Vec::new()
        },
        validation: vec![ReviewValidation {
            command: "make ci-fast".to_string(),
            outcome: ValidationOutcome::Passed,
            role: ValidationRole::Required,
            note: None,
            check: None,
        }],
        escalation: (verdict == ReviewVerdict::ChangesRequired)
            .then(|| "decide whether the note is required".to_string()),
    }
}

/// One classified validation record for a reviewer report.
pub(super) fn validation(
    command: &str,
    outcome: ValidationOutcome,
    role: ValidationRole,
    note: Option<&str>,
) -> ReviewValidation {
    ReviewValidation {
        command: command.to_string(),
        outcome,
        role,
        note: note.map(ToString::to_string),
        check: None,
    }
}

/// Persist the reviewer's report artifact the way the reviewer tool does.
pub(super) fn write_report(runtime: &OrbitRuntime, task_id: &str, report: &ReviewReport) {
    runtime
        .update_task(
            task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: REVIEW_REPORT_ARTIFACT.to_string(),
                    content: serde_json::to_vec(report).expect("serialize report"),
                    media_type: "application/json".to_string(),
                    created_by: None,
                }],
                ..TaskUpdateParams::default()
            },
        )
        .expect("write review report");
}
