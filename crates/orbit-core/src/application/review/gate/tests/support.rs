//! The gated fixture every gate test starts from.
//!
//! Fixtures build a real workspace runtime over a git checkout so admission
//! capture, the gate's Git evidence and ledger budgets exercise the production
//! paths [ORB-11333].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::Utc;
use orbit_engine::{ReviewReleaseRequest, RuntimeHost};
use orbit_types::task::{Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{
    FindingDisposition, JobRun, PipelineState, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewCertificate, ReviewFinding, ReviewLedger, ReviewReport,
    ReviewValidation, ReviewVerdict, ValidationOutcome, ValidationRole,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_config;
use crate::application::job::seed_default_jobs;
use crate::application::review::{
    install_review_admission, release_review_attempt, review_gate_admit, review_gate_settle,
};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

/// Workspace config with a before-PR reviewer crew distinct from the
/// implementer; tests append further `[operation]` keys.
pub(super) const BEFORE_PR: &str = "[crews.reviewers]\nmodel = \"review-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[crews.implementer]\nmodel = \"impl-model\"\nprovider = \"codex\"\nbackend = \"cli\"\n[workflow]\ndefault_crew = \"implementer\"\n[operation]\nreview_policy = \"before-pr\"\nreview_crew = \"reviewers\"\n";

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
    install_review_admission(runtime, job, &mut input, None, false).expect("capture admission");
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

/// Persist the reviewer's report artifact the way the reviewer tool does.
pub(super) fn write_report(runtime: &OrbitRuntime, task_id: &str, report: &ReviewReport) {
    write_report_bytes(
        runtime,
        task_id,
        serde_json::to_vec(report).expect("serialize report"),
    );
}

/// Persist report bytes exactly as given, drifted or malformed.
pub(super) fn write_report_bytes(runtime: &OrbitRuntime, task_id: &str, content: Vec<u8>) {
    runtime
        .update_task(
            task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![TaskArtifact {
                    path: REVIEW_REPORT_ARTIFACT.to_string(),
                    content,
                    media_type: "application/json".to_string(),
                    created_by: None,
                }],
                ..TaskUpdateParams::default()
            },
        )
        .expect("write review report");
}

/// Fixture with a task bundle, its admitted run, and a checked-out
/// candidate. `task_id` is the bundle's first task.
pub(super) struct Gated {
    pub(super) fixture: Fixture,
    pub(super) task_id: String,
    pub(super) bundle: Vec<String>,
    pub(super) run_id: String,
    pub(super) implementation_sha: String,
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
    let implementation_sha = implement_candidate(&fixture.repo, &bundle[0]);
    Gated {
        fixture,
        task_id: bundle[0].clone(),
        bundle,
        run_id: run.run_id,
        implementation_sha,
    }
}

impl Gated {
    /// A resume of `source`: a new run carrying the source's input, linked
    /// by `retry_source_run_id`, started now and owning the bundle.
    pub(super) fn resume(&self, source: &str) -> String {
        let runtime = &self.fixture.runtime;
        let input = runtime
            .get_job_run_backend(source)
            .expect("read source run")
            .and_then(|run| run.input);
        let run = RuntimeHost::insert_job_run(
            runtime,
            "task_pr_pipeline",
            2,
            Utc::now(),
            input,
            Some(source.to_string()),
        )
        .expect("insert resumed run");
        self.start(&run.run_id, Utc::now());
        bind_tasks(runtime, &self.bundle, &run.run_id);
        run.run_id
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

    /// Admit the re-review that follows `complete_pr` in the delivery run.
    pub(super) fn re_admit(&self, completion: &str) -> Result<Value, orbit_engine::DispatchError> {
        review_gate_admit(
            &self.fixture.runtime,
            "review_gate_admit",
            &json!({
                "job_run_id": self.run_id,
                "completed_task_ids": self.bundle,
                "workspace_path": self.fixture.repo,
                "base": "main",
                "base_sync": "local",
                "mode": "pr",
                "completion": completion,
                "skipped_no_diff_expected": false,
                "re_review_after": "complete_pr",
                "allowed_crews": [],
            }),
        )
    }

    /// Checkpoint `output` as the delivery run's `complete_pr` step.
    pub(super) fn record_completion(&self, output: Value) {
        let mut state = PipelineState::new(
            self.run_id.clone(),
            "task_pr_pipeline".to_string(),
            Value::Null,
        );
        state.record_pipeline_output("complete_pr", output);
        self.fixture
            .runtime
            .stores()
            .jobs()
            .write_run_state(&self.run_id, &state)
            .expect("checkpoint complete_pr");
    }

    pub(super) fn settle(&self, admission: &Value) -> Result<Value, orbit_engine::DispatchError> {
        self.settle_in(&self.run_id, admission)
    }

    pub(super) fn settle_in(
        &self,
        run_id: &str,
        admission: &Value,
    ) -> Result<Value, orbit_engine::DispatchError> {
        review_gate_settle(
            &self.fixture.runtime,
            "review_gate_settle",
            &json!({
                "job_run_id": run_id,
                "completed_task_ids": self.bundle,
                "workspace_path": self.fixture.repo,
                "base": "main",
                "base_sync": "local",
                "admission": admission,
            }),
        )
    }

    /// The lineage ledger an admission reserved in.
    pub(super) fn ledger(&self, admission: &Value) -> ReviewLedger {
        let runtime = &self.fixture.runtime;
        runtime
            .review_store()
            .expect("store")
            .review_ledger(
                &runtime.workspace_id().expect("workspace"),
                admission["lineage_key"].as_str().expect("lineage key"),
            )
            .expect("read ledger")
            .expect("ledger exists")
    }

    /// Release the attempt `admission` reserved, as the failure handoff of
    /// `run_id` does.
    pub(super) fn release(&self, run_id: &str, admission: &Value) {
        release_review_attempt(
            &self.fixture.runtime,
            &ReviewReleaseRequest {
                run_id: run_id.to_string(),
                lineage_key: admission["lineage_key"].as_str().expect("lineage").into(),
                attempt_id: admission["attempt_id"].as_str().expect("attempt").into(),
            },
        )
        .expect("release attempt");
    }

    pub(super) fn certificate(&self) -> ReviewCertificate {
        let artifact = self
            .fixture
            .runtime
            .get_task_artifact(&self.task_id, REVIEW_GATE_ARTIFACT)
            .expect("read")
            .expect("certificate artifact");
        serde_json::from_slice(&artifact.content).expect("certificate json")
    }
}
