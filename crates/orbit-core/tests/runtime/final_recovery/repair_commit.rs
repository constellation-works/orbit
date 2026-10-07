//! A recovery commit is a new candidate, with an exact host-recorded HEAD.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, RuntimeHost, V2AuditWriter, execute_job_with_resume,
};
use orbit_tools::ToolContext;
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::{FinalRecoveryDecision, PipelineState, ReviewManifest};
use serde_json::{Value, json};

use super::super::review_gate_audit::Fixture;

fn git(fixture: &Fixture, args: &[&str]) -> String {
    let mut command = std::process::Command::new("git");
    orbit_common::test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    let output = command
        .args(args)
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn fixture() -> Fixture {
    let mut fixture = Fixture::new_with_required_commands(&["true"]);
    let path = fixture.repo.join(".orbit/config.toml");
    let config = std::fs::read_to_string(&path).unwrap().replace(
        "default_crew = \"reviewers\"",
        "default_crew = \"reviewers\"\nfinal_recovery_crews = [\"reviewers\"]",
    );
    std::fs::write(path, config).unwrap();
    fixture.runtime = orbit_core::OrbitRuntime::from_roots(
        &fixture.runtime.paths().global_dir,
        &fixture.repo.join(".orbit"),
    )
    .unwrap();
    // Leave the implementation dirty so the first commit is the normal
    // pipeline commit. Recovery will append a repair to that candidate.
    git(&fixture, &["reset", "main"]);
    fixture
        .runtime
        .run_tool(
            "orbit.task.update",
            json!({
                "id": fixture.task_id, "model": "codex",
                "execution_summary": "Outcome: success\nChanges: repaired candidate behavior.",
            }),
        )
        .unwrap();
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let record = fixture.runtime.get_job_run(run).unwrap().unwrap();
    fixture
        .runtime
        .mark_job_run_running(run, Utc::now(), std::process::id())
        .unwrap();
    fixture
        .runtime
        .write_run_state(
            run,
            &PipelineState::new(run.into(), record.job_id, record.input.unwrap()),
        )
        .unwrap();
    fixture.input["base_sha"] = json!(git(&fixture, &["rev-parse", "main"]));
    fixture
}

struct RepairPipeline<'a> {
    fixture: &'a Fixture,
    admissions: Mutex<Vec<Value>>,
    commits: Mutex<Vec<Value>>,
    validations: Mutex<Vec<Value>>,
}

impl RuntimeHost for RepairPipeline<'_> {
    fn list_run_tasks(&self, run: &str) -> Result<Vec<orbit_types::task::Task>, OrbitError> {
        self.fixture.runtime.list_run_tasks(run)
    }

    fn get_task(&self, task: &str) -> Result<orbit_types::task::Task, OrbitError> {
        self.fixture.runtime.get_task(task)
    }

    fn read_run_state(&self, run: &str) -> Result<Option<PipelineState>, OrbitError> {
        self.fixture.runtime.read_run_state(run)
    }

    fn required_validation_commands(&self) -> Vec<String> {
        self.fixture.runtime.required_validation_commands()
    }

    fn attach_task_validation_log(
        &self,
        task: &str,
        run: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        self.fixture
            .runtime
            .attach_task_validation_log(task, run, path, content)
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        context: ToolContext,
    ) -> Result<Value, DispatchError> {
        match action {
            "setup" => Ok(self.fixture.input.clone()),
            "review_report" => {
                let admissions = self.admissions.lock().unwrap();
                let admission = admissions.last().unwrap();
                let verdict = if admissions.len() == 1 {
                    "reject"
                } else {
                    "accept"
                };
                self.fixture.put_report(&json!({
                    "schema_version": 1, "attempt_id": admission["attempt_id"],
                    "verdict": verdict, "summary": "Inspected candidate behavior.",
                    "findings": if verdict == "reject" { json!([{
                        "id": "F1", "severity": "high", "summary": "Candidate needs repair",
                        "paths": ["candidate.txt"], "disposition": {"kind": "open"},
                    }]) } else { json!([]) },
                    "validation": [{"command": "true", "outcome": "passed", "role": "required"}],
                    "escalation": if verdict == "reject" { Some("Repair F1") } else { None },
                }));
                Ok(json!({"verdict": verdict}))
            }
            "decide" => {
                std::fs::write(self.fixture.repo.join("candidate.txt"), "repaired\n").unwrap();
                git(self.fixture, &["add", "candidate.txt"]);
                git(self.fixture, &["commit", "-m", "repair rejected candidate"]);
                // Deliberately name settlement: the engine must rewind far
                // enough to admit a fresh reviewer for this repair.
                Ok(
                    json!({"decision": "resume", "step_id": "review_gate_settle",
                    "rationale": "repaired the rejected candidate"}),
                )
            }
            _ => {
                let result = self
                    .fixture
                    .runtime
                    .run_deterministic(action, config, input, context)?;
                if action == "review_gate_admit" {
                    self.admissions.lock().unwrap().push(result.clone());
                }
                Ok(result)
            }
        }
    }

    fn checkpoint_step(
        &self,
        run_id: &str,
        index: u32,
        step_id: &str,
        output: &Value,
        compound: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        if step_id == "commit" {
            self.commits.lock().unwrap().push(output.clone());
        } else if step_id == "validate" {
            self.validations.lock().unwrap().push(output.clone());
        }
        self.fixture
            .runtime
            .checkpoint_step(run_id, index, step_id, output, compound)
    }

    fn final_recovery_log_tail(&self, run: &str) -> Result<Option<String>, OrbitError> {
        self.fixture.runtime.final_recovery_log_tail(run)
    }

    fn admit_final_recovery(
        &self,
        run: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        self.fixture.runtime.admit_final_recovery(run, request)
    }

    fn apply_final_recovery(
        &self,
        run: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        RuntimeHost::apply_final_recovery(&self.fixture.runtime, run, application)
    }
}

fn job(fixture: &Fixture) -> JobV2 {
    let mut input = fixture.input.clone();
    input["admission"] = json!("{{ steps.review_gate_admit.output }}");
    let step = |id: &str, action: &str| {
        json!({
            "id": id, "default_input": if id == "review_gate_settle" { &input } else { &fixture.input },
            "spec": {"type": "deterministic", "action": action, "config": {}},
        })
    };
    let mut job = load_job_asset(&json!({
        "schemaVersion": 2, "kind": "Job", "metadata": {"name": "recovery_repair"},
        "spec": {"state": "enabled", "kind": "workflow", "steps": [
            step("worktree", "setup"), step("commit", "git_commit"),
            step("validate", "candidate_validate"), step("review_gate_admit", "review_gate_admit"),
            step("review", "review_report"), step("review_gate_settle", "review_gate_settle"),
        ]},
    }).to_string()).unwrap().spec;
    job.final_recovery_activity = Some("decide".into());
    job.resolved_final_recovery_activity = Some(
        load_activity_asset(
            &json!({
                "schemaVersion": 2, "kind": "Activity", "metadata": {"name": "decide"},
                "spec": {"type": "deterministic", "description": "Repair rejected candidate",
                    "action": "decide", "config": {}},
            })
            .to_string(),
        )
        .unwrap()
        .spec,
    );
    job
}

fn recover(fixture: &Fixture) {
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let host = RepairPipeline {
        fixture,
        admissions: Mutex::new(Vec::new()),
        commits: Mutex::new(Vec::new()),
        validations: Mutex::new(Vec::new()),
    };
    let audit = V2AuditWriter::with_disk_sinks(
        &fixture.repo.join(".orbit/tmp/audit"),
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws_fixture",
        run,
        "fixture",
        Some(&fixture.repo),
    )
    .unwrap();
    let input = fixture
        .runtime
        .get_job_run(run)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let outcome = execute_job_with_resume(&job(fixture), input, run, audit, &host, None).unwrap();
    assert!(outcome.success, "{outcome:?}");
    let commits = host.commits.lock().unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0]["decision"], "performed");
    assert_eq!(commits[1]["decision"], "already_committed");
    let admissions = host.admissions.lock().unwrap();
    assert_eq!(
        admissions.len(),
        2,
        "the recovery repair requires a fresh before-PR review"
    );
    assert_ne!(admissions[0]["attempt_id"], admissions[1]["attempt_id"]);
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    assert!(
        matches!(checkpoint.decision, Some(FinalRecoveryDecision::Resume { ref step_id, .. }) if step_id == "commit")
    );
    let repair = checkpoint.repair_commit.unwrap();
    assert_eq!(
        repair.head_sha_before,
        commits[0]["commit_sha"].as_str().unwrap()
    );
    assert_eq!(repair.head_sha, git(fixture, &["rev-parse", "HEAD"]));
    let validations = host.validations.lock().unwrap();
    assert_eq!(
        validations.len(),
        2,
        "both candidates must pass required validation"
    );
    assert_eq!(validations[0]["tested_head"], repair.head_sha_before);
    assert_eq!(validations[1]["tested_head"], repair.head_sha);
    assert!(
        validations
            .iter()
            .all(|output| output["decision"] == "passed")
    );
    let manifest: ReviewManifest = serde_json::from_slice(
        &fixture
            .runtime
            .get_task_artifact(&fixture.task_id, "review-manifest.json")
            .unwrap()
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(manifest.candidate.commit, repair.head_sha);
    assert_ne!(manifest.candidate.commit, repair.head_sha_before);
    assert_eq!(outcome.pipeline["review_gate_settle"]["gate"], "passed");
}

#[test]
fn final_recovery_repair_commit_gets_a_fresh_before_pr_review() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::repair_commit::final_recovery_repair_commit_gets_a_fresh_before_pr_review",
    ) {
        return;
    }
    recover(&fixture());
}

#[test]
fn final_recovery_does_not_authorize_an_unrecorded_head_change() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::repair_commit::final_recovery_does_not_authorize_an_unrecorded_head_change",
    ) {
        return;
    }
    for recorded in [false, true] {
        let fixture = fixture();
        if recorded {
            recover(&fixture);
        }
        std::fs::write(fixture.repo.join("candidate.txt"), "unrecorded change\n").unwrap();
        git(&fixture, &["add", "candidate.txt"]);
        git(&fixture, &["commit", "-m", "unrecorded HEAD change"]);
        let error = orbit_engine::execute_deterministic_action(
            &fixture.runtime,
            "git_commit",
            &json!({}),
            &fixture.input,
            false,
            &Default::default(),
            None,
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("worktree_head_changed"),
            "unrecorded HEAD changes fail even after a recorded recovery: {error}"
        );
    }
}
