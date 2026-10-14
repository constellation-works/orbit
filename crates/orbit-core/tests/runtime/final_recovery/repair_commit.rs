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
    let mut command = crate::git_test_support::command();
    let output = command
        .args(args)
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn fixture() -> Fixture {
    fixture_with_command("true")
}

fn fixture_with_command(command: &str) -> Fixture {
    let mut fixture = Fixture::new_with_required_commands(&[command]);
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
    repair: &'static str,
    decisions: Mutex<usize>,
    validation_heads: Mutex<Vec<(String, String)>>,
    admissions: Mutex<Vec<Value>>,
    commits: Mutex<Vec<Value>>,
    validations: Mutex<Vec<Value>>,
}

impl orbit_engine::test_support::NullRuntimeHost for RepairPipeline<'_> {
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
            "setup" => {
                let mut output = self.fixture.input.clone();
                if self.repair == "foreign-root" {
                    let nested = self.fixture.repo.join("nested");
                    std::fs::create_dir(&nested).unwrap();
                    output["workspace_path"] = json!(nested);
                } else if self.repair == "unrelated-base" {
                    output["base_sha"] = json!(git(
                        self.fixture,
                        &["commit-tree", "HEAD^{tree}", "-m", "unrelated root"]
                    ));
                }
                Ok(output)
            }
            "pin_review" => Ok(if self.repair == "settled-review" {
                json!({"reviewed_head_sha": git(self.fixture, &["rev-parse", "HEAD"])})
            } else {
                json!({"applies": true, "attempt_id": "admitted", "head_sha": git(self.fixture, &["rev-parse", "HEAD"])})
            }),
            "review_report" => {
                let admissions = self.admissions.lock().unwrap();
                let admission = admissions.last().unwrap();
                let verdict = if self.repair == "committed" && admissions.len() == 1 {
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
                    "validation": [{"id": "V1", "command": self.fixture.runtime.required_validation_commands()[0], "outcome": "passed", "role": "required"}],
                    "escalation": if verdict == "reject" { Some("Repair F1") } else { None },
                }));
                Ok(json!({"verdict": verdict}))
            }
            "decide" => {
                *self.decisions.lock().unwrap() += 1;
                if self.repair != "committed" {
                    assert_eq!(input["failed_step_id"], "validate");
                    self.validation_heads.lock().unwrap().push((
                        git(self.fixture, &["rev-parse", "HEAD"]),
                        git(self.fixture, &["status", "--porcelain"]),
                    ));
                }
                std::fs::write(self.fixture.repo.join("candidate.txt"), "repaired\n").unwrap();
                match self.repair {
                    "committed" => {
                        git(self.fixture, &["add", "candidate.txt"]);
                        git(self.fixture, &["commit", "-m", "repair rejected candidate"]);
                    }
                    "protected" => {
                        std::fs::write(self.fixture.repo.join(".env"), "TOKEN=x\n").unwrap()
                    }
                    #[cfg(unix)]
                    "symlink" => {
                        std::os::unix::fs::symlink("candidate.txt", self.fixture.repo.join("link"))
                            .unwrap()
                    }
                    "foreign-branch" => {
                        git(self.fixture, &["checkout", "-b", "foreign"]);
                    }
                    "foreign-head" => {
                        git(
                            self.fixture,
                            &["commit", "--allow-empty", "-m", "foreign head"],
                        );
                    }
                    _ => {}
                }
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
            self.validation_heads.lock().unwrap().push((
                output["tested_head"].as_str().unwrap().into(),
                git(self.fixture, &["status", "--porcelain"]),
            ));
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

fn job(fixture: &Fixture, repair: &str) -> JobV2 {
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
    if matches!(repair, "reviewed" | "settled-review") {
        let mut pin = job.steps[0].clone();
        pin.id = "pin_review".into();
        let orbit_types::workflow::activity_job::JobV2StepBody::Target(target) = &mut pin.body
        else {
            unreachable!()
        };
        let orbit_types::workflow::activity_job::ActivityV2Spec::Deterministic(spec) =
            &mut target.spec
        else {
            unreachable!()
        };
        spec.action = "pin_review".into();
        job.steps.insert(2, pin);
    }
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

fn run_pipeline<'a>(
    fixture: &'a Fixture,
    repair: &'static str,
) -> (
    RepairPipeline<'a>,
    Result<orbit_engine::JobOutcome, DispatchError>,
) {
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let host = RepairPipeline {
        fixture,
        repair,
        decisions: Mutex::new(0),
        validation_heads: Mutex::new(Vec::new()),
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
    let mut resume = None;
    if matches!(repair, "no-diff" | "foreign-task") {
        // Exercise authoritative checkpoint refusals without changing task
        // policy or teaching the deterministic commit action a fixture hook.
        let mut committed = orbit_engine::execute_deterministic_action(
            &fixture.runtime,
            "git_commit",
            &json!({}),
            &fixture.input,
            false,
            &Default::default(),
            None,
        )
        .unwrap();
        if repair == "no-diff" {
            committed["skipped_no_diff_expected"] = json!(true);
        } else {
            committed["task_id"] = json!("unrelated-task");
        }
        host.commits.lock().unwrap().push(committed.clone());
        let mut state = PipelineState::new(run.into(), "recovery_repair".into(), input.clone());
        for (index, output) in [(0, fixture.input.clone()), (1, committed)] {
            state
                .step_states
                .insert(index, orbit_types::workflow::JobRunState::Success);
            state.step_outputs.insert(index, output);
        }
        resume = Some(state);
    }
    let outcome = execute_job_with_resume(
        &job(fixture, repair),
        input,
        run,
        audit,
        &host,
        resume.as_ref(),
    );
    (host, outcome)
}

fn recover(fixture: &Fixture) {
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let (host, outcome) = run_pipeline(fixture, "committed");
    let outcome = outcome.unwrap();
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

fn final_recovery_repair_commit_gets_a_fresh_before_pr_review() {
    recover(&fixture());

    // The provider only edits files, as in the read-only Git sandbox. A real
    // required command fails on the implementation and passes on the base.
    // Recovery must adopt its repair before the real validator runs again.
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let (host, outcome) = run_pipeline(&fixture, "files");
    let outcome = outcome.unwrap();
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(
        *host.decisions.lock().unwrap(),
        1,
        "F2026-10-345: no second recovery invocation to adopt dirty files"
    );
    let heads = host.validation_heads.lock().unwrap();
    assert_eq!(
        heads.len(),
        2,
        "both candidate heads reach committed-candidate validation"
    );
    assert_ne!(heads[0].0, heads[1].0);
    assert!(heads.iter().all(|(_, status)| status.is_empty()));
    assert_eq!(
        git(&fixture, &["rev-parse", &format!("{}^", heads[1].0)]),
        heads[0].0
    );
    let commits = host.commits.lock().unwrap();
    assert_eq!(
        commits.len(),
        2,
        "the repair reruns the guarded commit boundary"
    );
    assert_eq!(commits[1]["decision"], "already_committed");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let state = fixture.runtime.read_run_state(run).unwrap().unwrap();
    let checkpoint = state.final_recovery.unwrap();
    assert!(
        matches!(checkpoint.decision, Some(FinalRecoveryDecision::Resume { ref step_id, .. }) if step_id == "commit")
    );
    let repair = checkpoint.repair_commit.unwrap();
    assert_eq!(repair.workspace_path, fixture.repo.canonicalize().unwrap());
    assert_eq!(repair.head_sha_before, heads[0].0);
    assert_eq!(repair.head_sha, heads[1].0);
    assert_eq!(
        host.validations.lock().unwrap()[0]["tested_head"],
        repair.head_sha
    );
    let admissions = host.admissions.lock().unwrap();
    assert_eq!(
        admissions.len(),
        1,
        "the new head requires review admission"
    );
    assert_eq!(admissions[0]["head_sha"], repair.head_sha);
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
    assert_eq!(outcome.pipeline["review_gate_settle"]["gate"], "passed");
    let message = git(&fixture, &["show", "-s", "--format=%B", &repair.head_sha]);
    assert!(
        message.contains(&format!("Orbit-Recovery-Run: {run}"))
            && message.contains("Orbit-Recovery-Step: validate")
            && message.contains("Orbit-Recovery-Activity: decide"),
        "host adoption keeps recovery provenance: {message}"
    );
}

fn final_recovery_does_not_authorize_an_unrecorded_head_change() {
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

    for (case, reason) in [
        ("protected", "protected_path"),
        #[cfg(unix)]
        ("symlink", "outside_footprint"),
        ("no-diff", "no_diff_route"),
        ("reviewed", "reviewed_candidate"),
        ("settled-review", "reviewed_candidate"),
        ("foreign-task", "uncommittable"),
        ("foreign-root", "uncommittable"),
        ("foreign-branch", "uncommittable"),
        ("foreign-head", "uncommittable"),
        ("unrelated-base", "uncommittable"),
    ] {
        let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
        let (host, outcome) = run_pipeline(&fixture, case);
        assert!(outcome.is_err(), "{case}: {outcome:?}");
        assert_eq!(
            *host.decisions.lock().unwrap(),
            1,
            "{case}: recovery stays bounded"
        );
        assert_eq!(
            host.validation_heads.lock().unwrap().len(),
            1,
            "{case}: refused repairs never reach validation"
        );
        assert_eq!(
            host.commits.lock().unwrap().len(),
            1,
            "{case}: no commit retry"
        );
        assert!(
            host.admissions.lock().unwrap().is_empty(),
            "{case}: no review admission"
        );
        assert_eq!(
            git(&fixture, &["diff", "--cached", "--name-only"]),
            "",
            "{case}: index unchanged"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.repo.join("candidate.txt")).unwrap(),
            "repaired\n",
            "{case}: preserve the refused files"
        );
        let initial = host.validation_heads.lock().unwrap()[0].0.clone();
        if case != "foreign-head" {
            assert_eq!(
                git(&fixture, &["rev-parse", "HEAD"]),
                initial,
                "{case}: no adoption commit"
            );
        } else {
            assert_eq!(
                git(&fixture, &["rev-parse", "HEAD^"]),
                initial,
                "{case}: only the scripted foreign commit exists"
            );
        }
        let run = fixture.input["job_run_id"].as_str().unwrap();
        let checkpoint = fixture
            .runtime
            .read_run_state(run)
            .unwrap()
            .unwrap()
            .final_recovery
            .unwrap();
        assert!(
            checkpoint.repair_commit.is_none(),
            "{case}: no authority for a moved head"
        );
        assert!(
            matches!(checkpoint.decision, Some(FinalRecoveryDecision::Escalate { ref diagnosis, .. }) if diagnosis.contains(&format!("recovery_commit_refused:{reason}:"))),
            "{case}: durable refusal: {checkpoint:?}"
        );
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    final_recovery_repair_commit_gets_a_fresh_before_pr_review_scenarios,
    final_recovery_repair_commit_gets_a_fresh_before_pr_review,
    final_recovery_does_not_authorize_an_unrecorded_head_change
);
