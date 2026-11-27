//! A recovery commit is a new candidate, with an exact host-recorded HEAD.

mod evidence;

use orbit_engine::test_support::impl_test_host;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, V2AuditWriter, execute_job_with_resume,
};
use orbit_engine::{HostEnvironment, Recovery, RunLifecycle, RuntimeHostRunSettings};
use orbit_tools::ToolContext;
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::{
    FinalRecoveryCheckpointOutcome, FinalRecoveryDecision, JobRunState, PipelineState,
    ReviewManifest,
};
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

/// Whether the repository object database holds `path`'s current bytes.
fn blob_in_object_store(fixture: &Fixture, path: &str) -> bool {
    let blob = git(fixture, &["hash-object", path]);
    crate::git_test_support::command()
        .args(["cat-file", "-e", &blob])
        .current_dir(&fixture.repo)
        .output()
        .unwrap()
        .status
        .success()
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
    fixture.runtime = orbit_core::runtime::OrbitRuntime::from_roots(
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
    let record = orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .get_job_run(run)
        .unwrap()
        .unwrap();
    orbit_core::test_support::EngineHost::new(&fixture.runtime)
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

impl_test_host! {
    impl Dispatch for RepairPipeline<'_> {
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
                    crate::review_admission_shape::admitted_review(
                        "admitted",
                        "lineage",
                        &git(self.fixture, &["rev-parse", "HEAD"]),
                    )
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
                        "validation": [{
                            "id": "V1",
                            "command":
                                orbit_core::test_support::EngineHost::new(&self.fixture.runtime)
                                    .required_validation_commands()[0],
                            "outcome": "passed",
                            "role": "required",
                        }],
                        "escalation": if verdict == "reject" { Some("Repair F1") } else { None },
                    }));
                    Ok(json!({"verdict": verdict}))
                }
                "decide" => {
                    *self.decisions.lock().unwrap() += 1;
                    if self.repair != "committed" {
                        let expected_failed_step = if self.repair == "landing-review" {
                            "landing_review"
                        } else {
                            "validate"
                        };
                        assert_eq!(input["failed_step_id"], expected_failed_step);
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
                    "protected" | "evidence-protected" | "scratch-ok" | "scratch-redirect" => {
                        std::fs::write(
                                self.fixture.repo.join("canary.txt"),
                                "ordinary safe line\napi_key=synthetic_patch_api_canary\nAuthorization: Bearer synthetic_patch_bearer_canary\nsk-syntheticpatchcanary00000000\n",
                            )
                            .unwrap();
                        std::fs::write(
                            self.fixture.repo.join(".env"),
                            "ENV_CANARY=synthetic_env_file_canary\n",
                        )
                        .unwrap();
                    }
                    "transition" | "scratch-transition" => {
                        let repo = &self.fixture.repo;
                        std::fs::remove_file(repo.join("to-dir")).unwrap();
                        std::fs::create_dir(repo.join("to-dir")).unwrap();
                        std::fs::write(repo.join("to-dir/.env"), "new_protected_canary\n")
                            .unwrap();
                        std::fs::write(repo.join("to-dir/ordinary"), "new ordinary content\n")
                            .unwrap();
                        std::fs::remove_file(repo.join("to-file/.env")).unwrap();
                        std::fs::remove_dir(repo.join("to-file")).unwrap();
                        std::fs::write(repo.join("to-file"), "replacement ordinary content\n")
                            .unwrap();
                    }
                    "reviewed-binary" => {
                        let repo = &self.fixture.repo;
                        std::fs::write(
                            repo.join("tracked.bin"),
                            b"\x00\x89PNG\r\n\x1a\n\x00\xff\xfe\x04",
                        )
                        .unwrap();
                        std::fs::write(repo.join("raw.dat"), b"hdr\x80\x81\xff\n").unwrap();
                        std::fs::write(
                            repo.join("trailing.txt"),
                            b"first line\nfinal spaces   \n",
                        )
                        .unwrap();
                        std::fs::write(repo.join("no_nl.txt"), b"no newline   ").unwrap();
                        std::fs::write(repo.join("added.bin"), b"\x7fELF\x00\x01\x02\xff")
                            .unwrap();
                    }
                    #[cfg(unix)]
                    "symlink" => std::os::unix::fs::symlink(
                        "candidate.txt",
                        self.fixture.repo.join("link"),
                    )
                    .unwrap(),
                    #[cfg(unix)]
                    "non-utf8" => {
                        use std::ffi::OsStr;
                        use std::os::unix::ffi::OsStrExt;

                        let repo = &self.fixture.repo;
                        std::fs::write(
                            repo.join(OsStr::from_bytes(b"loose-\xff.pyc")),
                            b"\x00nonutf8\xff",
                        )
                        .unwrap();
                        let nested = repo.join(OsStr::from_bytes(b"dir-\xfe"));
                        std::fs::create_dir(&nested).unwrap();
                        std::fs::write(
                            nested.join(OsStr::from_bytes(b"sub-\xfd.txt")),
                            b"native path content\n",
                        )
                        .unwrap();
                        std::fs::write(
                            nested.join(".env"),
                            b"ENV_CANARY=non_utf8_dir_secret\n",
                        )
                        .unwrap();
                        std::fs::write(repo.join(".env"), b"ENV_CANARY=top_secret\n").unwrap();
                    }
                    #[cfg(unix)]
                    "worktree-rename" => {
                        use std::ffi::OsStr;
                        use std::os::unix::ffi::OsStrExt;

                        // A worktree-side rename: the source is deleted on disk and the
                        // destination is intent-to-add, so porcelain reports ` R` with no
                        // index-side status.
                        let repo = &self.fixture.repo;
                        std::fs::rename(
                            repo.join(OsStr::from_bytes(b"rename-src-\xfe.txt")),
                            repo.join("renamed.txt"),
                        )
                        .unwrap();
                        git(self.fixture, &["add", "-N", "renamed.txt"]);
                    }
                    "staged-ignored" => {
                        let repo = &self.fixture.repo;
                        std::fs::write(repo.join("notes.ignored"), "initial ignored\n")
                            .unwrap();
                        std::fs::create_dir_all(repo.join("ignored-dir")).unwrap();
                        std::fs::write(
                            repo.join("ignored-dir/nested.txt"),
                            "nested ignored content\n",
                        )
                        .unwrap();
                        std::fs::write(
                            repo.join("ignored-dir/untracked.txt"),
                            "must not stage\n",
                        )
                        .unwrap();
                        std::fs::write(repo.join("deleted.ignored"), "gone before capture\n")
                            .unwrap();
                        std::fs::write(
                            repo.join(".env"),
                            "ENV_CANARY=synthetic_ignored_env_canary\n",
                        )
                        .unwrap();
                        std::fs::write(
                            repo.join(".env.local"),
                            "ENV_LOCAL_CANARY=synthetic_force_added_env_canary\n",
                        )
                        .unwrap();
                        git(
                            self.fixture,
                            &[
                                "add",
                                "-f",
                                "notes.ignored",
                                "ignored-dir/nested.txt",
                                "deleted.ignored",
                                ".env.local",
                            ],
                        );
                        std::fs::write(
                            repo.join("notes.ignored"),
                            "staged ignored content\nsk-syntheticignoredcanary0000\n",
                        )
                        .unwrap();
                        std::fs::remove_file(repo.join("deleted.ignored")).unwrap();
                    }
                    "staged-only" => {
                        let repo = &self.fixture.repo;
                        std::fs::write(
                            repo.join("candidate.txt"),
                            "staged repair\nsk-syntheticstagedcanary0000\n",
                        )
                        .unwrap();
                        std::fs::write(repo.join("staged_deleted.txt"), "staged new file\n")
                            .unwrap();
                        git(
                            self.fixture,
                            &["add", "candidate.txt", "staged_deleted.txt"],
                        );
                        // Restore `candidate.txt` on disk to HEAD (`"after\n"`) and remove
                        // `staged_deleted.txt`, leaving both changes only in the live index.
                        std::fs::write(repo.join("candidate.txt"), "after\n").unwrap();
                        std::fs::remove_file(repo.join("staged_deleted.txt")).unwrap();
                        std::fs::write(
                            repo.join(".env"),
                            "ENV_CANARY=synthetic_staged_only_env_canary\n",
                        )
                        .unwrap();
                    }
                    "staged-mixed" => {
                        let repo = &self.fixture.repo;
                        std::fs::write(
                            repo.join("tracked_reverted.txt"),
                            "staged reverted edit\n",
                        )
                        .unwrap();
                        std::fs::write(repo.join("staged_deleted.txt"), "staged deleted new\n")
                            .unwrap();
                        std::fs::write(repo.join("both_changed.txt"), "staged intermediate\n")
                            .unwrap();
                        git(
                            self.fixture,
                            &[
                                "add",
                                "tracked_reverted.txt",
                                "staged_deleted.txt",
                                "both_changed.txt",
                            ],
                        );
                        std::fs::write(repo.join("tracked_reverted.txt"), "base\n").unwrap();
                        std::fs::remove_file(repo.join("staged_deleted.txt")).unwrap();
                        std::fs::write(repo.join("both_changed.txt"), "worktree final\n")
                            .unwrap();
                        std::fs::write(
                            repo.join(".env"),
                            "ENV_CANARY=synthetic_staged_mixed_env_canary\n",
                        )
                        .unwrap();
                    }
                    "staged-replaced-dir" => {
                        let repo = &self.fixture.repo;
                        std::fs::create_dir_all(repo.join("replaced-dir")).unwrap();
                        std::fs::write(
                            repo.join("replaced-dir/nested.txt"),
                            "nested staged content\n",
                        )
                        .unwrap();
                        git(self.fixture, &["add", "replaced-dir/nested.txt"]);
                        // Replace the staged parent directory on disk with a
                        // regular file: status reports `AD
                        // replaced-dir/nested.txt` alongside `??
                        // replaced-dir`, and `symlink_metadata` on the nested
                        // path fails with `ENOTDIR` (`NotADirectory`).
                        std::fs::remove_file(repo.join("replaced-dir/nested.txt")).unwrap();
                        std::fs::remove_dir(repo.join("replaced-dir")).unwrap();
                        std::fs::write(repo.join("replaced-dir"), "replacement file content\n")
                            .unwrap();
                        std::fs::write(
                            repo.join(".env"),
                            "ENV_CANARY=synthetic_replaced_dir_env_canary\n",
                        )
                        .unwrap();
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
                match self.repair {
                    "evidence" | "evidence-protected" => {
                        return Ok(evidence::repair_evidence_output(
                            self.fixture,
                            "review_gate_settle",
                        ));
                    }
                    "evidence-admission-step" => {
                        return Ok(evidence::repair_evidence_output(self.fixture, "worktree"));
                    }
                    "evidence-missing-step" => {
                        return Ok(evidence::repair_evidence_output(
                            self.fixture,
                            "no_such_step",
                        ));
                    }
                    "evidence-unattributed" => {
                        return Ok(evidence::unattributed_repair_output(self.fixture));
                    }
                    "evidence-bad-artifact" => {
                        return Ok(evidence::refused_artifact_output(self.fixture));
                    }
                    _ => {}
                }
                // Deliberately name settlement: the engine must rewind far
                // enough to admit a fresh reviewer for this repair.
                Ok(
                    json!({"decision": "resume", "fault": "implementer", "fault_evidence": "run fixture: the candidate failed its check", "step_id": "review_gate_settle",
                        "rationale": "repaired the rejected candidate"}),
                )
            }
            _ => {
                let mut result =
                    orbit_core::test_support::EngineHost::new(&self.fixture.runtime)
                        .run_deterministic(action, config, input, context)?;
                if action == "review_gate_admit" {
                    if self.repair == "landing-review" {
                        result["timing"] = json!("before-landing");
                    }
                    self.admissions.lock().unwrap().push(result.clone());
                }
                Ok(result)
            }
        }
    }
    }
}

impl_test_host! {
    impl HostEnvironment for RepairPipeline<'_> {
        fn required_validation_commands(&self) -> Vec<String> {
            orbit_core::test_support::EngineHost::new(&self.fixture.runtime)
                .required_validation_commands()
        }
    }
}

impl_test_host! {
    impl RuntimeHostRunSettings for RepairPipeline<'_> {
        fn run_settings(&self) -> std::sync::Arc<orbit_engine::RunSettings> {
            RuntimeHostRunSettings::run_settings(&orbit_core::test_support::EngineHost::new(
                &self.fixture.runtime,
            ))
        }
    }
}

impl_test_host! {
    impl RunLifecycle for RepairPipeline<'_> {
        fn list_run_tasks(&self, run: &str) -> Result<Vec<orbit_types::task::Task>, OrbitError> {
            self.fixture.runtime.list_run_tasks(run)
        }

        fn get_task(&self, task: &str) -> Result<orbit_types::task::Task, OrbitError> {
            self.fixture.runtime.get_task(task)
        }

        fn apply_task_automation_update(
            &self,
            task_id: &str,
            update: orbit_engine::TaskAutomationUpdate,
        ) -> Result<(), OrbitError> {
            if self.repair.starts_with("scratch-") && !update.upsert_artifacts.is_empty() {
                return Err(OrbitError::Execution("synthetic artifact refusal".into()));
            }
            orbit_core::test_support::EngineHost::new(&self.fixture.runtime)
                .apply_task_automation_update(task_id, update)
        }

        fn read_run_state(&self, run: &str) -> Result<Option<PipelineState>, OrbitError> {
            self.fixture.runtime.read_run_state(run)
        }

        fn get_job_run(
            &self,
            run: &str,
        ) -> Result<Option<orbit_types::workflow::JobRun>, OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.fixture.runtime).get_job_run(run)
        }
    }
}

impl_test_host! {
    impl Recovery for RepairPipeline<'_> {
        fn checkpoint_step(
            &self,
            run_id: &str,
            index: u32,
            step_id: &str,
            output: &Value,
            compound: &BTreeMap<String, Value>,
            script_outputs: &BTreeMap<String, orbit_types::workflow::activity_job::ScriptOutputSet>,
        ) -> Result<(), DispatchError> {
            if step_id == "commit" {
                self.commits.lock().unwrap().push(output.clone());
            } else if matches!(step_id, "validate" | "landing_review") {
                self.validations.lock().unwrap().push(output.clone());
                self.validation_heads.lock().unwrap().push((
                    output["tested_head"].as_str().unwrap().into(),
                    git(self.fixture, &["status", "--porcelain"]),
                ));
            }
            orbit_core::test_support::EngineHost::new(&self.fixture.runtime).checkpoint_step(
                run_id,
                index,
                step_id,
                output,
                compound,
                script_outputs,
            )
        }

        fn persist_final_recovery_evidence(
            &self,
            run_id: &str,
            task_id: &str,
            workspace: &std::path::Path,
            evidence: &Value,
        ) -> Result<(), OrbitError> {
            Recovery::persist_final_recovery_evidence(
                &orbit_core::test_support::EngineHost::new(&self.fixture.runtime),
                run_id,
                task_id,
                workspace,
                evidence,
            )
        }

        fn final_recovery_log_tail(&self, run: &str) -> Result<Option<String>, OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.fixture.runtime)
                .final_recovery_log_tail(run)
        }

        fn admit_final_recovery(
            &self,
            run: &str,
            request: &FinalRecoveryAdmissionRequest,
        ) -> Result<FinalRecoveryAdmission, OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.fixture.runtime)
                .admit_final_recovery(run, request)
        }

        fn apply_final_recovery(
            &self,
            run: &str,
            application: &FinalRecoveryApplication,
        ) -> Result<FinalRecoveryApplied, OrbitError> {
            Recovery::apply_final_recovery(
                &orbit_core::test_support::EngineHost::new(&self.fixture.runtime),
                run,
                application,
            )
        }
    }
}

impl_test_host! {
    impl ReviewGate for RepairPipeline<'_> {
        fn attach_task_validation_log(
            &self,
            task: &str,
            run: &str,
            path: &str,
            content: Vec<u8>,
        ) -> Result<(), OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.fixture.runtime)
                .attach_task_validation_log(task, run, path, content)
        }
    }
}

impl_test_host! {
    impl RuntimeHostProcessEnvironment for RepairPipeline<'_> {}
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
    if matches!(repair, "reviewed" | "settled-review" | "reviewed-binary") {
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
    if repair == "landing-review" {
        let admission = job
            .steps
            .iter()
            .position(|step| step.id == "review_gate_admit")
            .expect("the landing-review scenario has a review admission step");
        let validation = job
            .steps
            .iter()
            .position(|step| step.id == "validate")
            .expect("the landing-review scenario has a validation step");
        job.steps.swap(admission, validation);
        let validation = job
            .steps
            .iter_mut()
            .find(|step| step.id == "validate")
            .expect("the landing-review scenario has its validation step");
        validation.id = "landing_review".into();
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

fn repair_host<'a>(fixture: &'a Fixture, repair: &'static str) -> RepairPipeline<'a> {
    RepairPipeline {
        fixture,
        repair,
        decisions: Mutex::new(0),
        validation_heads: Mutex::new(Vec::new()),
        admissions: Mutex::new(Vec::new()),
        commits: Mutex::new(Vec::new()),
        validations: Mutex::new(Vec::new()),
    }
}

fn audit(fixture: &Fixture, run: &str) -> Arc<V2AuditWriter> {
    V2AuditWriter::with_disk_sinks(
        &fixture.repo.join(".orbit/tmp/audit"),
        orbit_store::Store::open_in_memory().unwrap(),
        "ws_fixture",
        run,
        "fixture",
        Some(&fixture.repo),
    )
    .unwrap()
}

fn run_pipeline<'a>(
    fixture: &'a Fixture,
    repair: &'static str,
) -> (
    RepairPipeline<'a>,
    Result<orbit_engine::JobOutcome, DispatchError>,
) {
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let host = repair_host(fixture, repair);
    let audit = audit(fixture, run);
    let input = orbit_core::test_support::EngineHost::new(&fixture.runtime)
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
            &orbit_core::test_support::EngineHost::new(&fixture.runtime),
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

fn final_recovery_commits_a_landing_review_repair_with_unsettled_admission() {
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let (host, outcome) = run_pipeline(&fixture, "landing-review");
    let outcome = outcome.unwrap();
    assert!(outcome.success, "{outcome:?}");
    assert_eq!(*host.decisions.lock().unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.repo.join("candidate.txt")).unwrap(),
        "repaired\n"
    );

    let run = fixture.input["job_run_id"].as_str().unwrap();
    let state = fixture.runtime.read_run_state(run).unwrap().unwrap();
    let checkpoint = state.final_recovery.unwrap();
    let repair = checkpoint
        .repair_commit
        .expect("the unsettled before-landing admission must not refuse recovery commit");
    assert_ne!(repair.head_sha, repair.head_sha_before);
    assert_eq!(repair.workspace_path, fixture.repo.canonicalize().unwrap());
    assert!(matches!(
        checkpoint.decision,
        Some(FinalRecoveryDecision::Resume { ref step_id, .. }) if step_id == "commit"
    ));
}

fn final_recovery_persists_repair_evidence_without_task_write_tools() {
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let (_, outcome) = run_pipeline(&fixture, "evidence");
    let outcome = outcome.unwrap();
    assert!(
        outcome.success,
        "repair evidence must survive the final-recovery tool denial: {outcome:?}"
    );
    assert_eq!(
        fixture
            .runtime
            .get_task(&fixture.task_id)
            .unwrap()
            .execution_summary,
        "Outcome: success\nChanges: repaired candidate with evidence."
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, "recovery/repair.log")
        .unwrap()
        .expect("the host attaches recovery evidence without operator action");
    assert_eq!(artifact.content, b"repair validation passed\n");
}

/// [ORB-27946] A resume whose answer lacks its fault attribution escalates
/// before its evidence reaches the host, so the prior summary and artifacts
/// stay as they were.
fn final_recovery_refuses_unattributed_repair_evidence_before_persisting() {
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let prior_summary = attach_prior_evidence(&fixture);
    let (_, outcome) = run_pipeline(&fixture, "evidence-unattributed");
    assert!(
        !outcome.as_ref().is_ok_and(|outcome| outcome.success),
        "a refused repair must not succeed: {outcome:?}"
    );
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let state = fixture.runtime.read_run_state(run).unwrap().unwrap();
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) =
        state.final_recovery.unwrap().decision
    else {
        panic!("a resume without its fault attribution escalates");
    };
    assert!(
        diagnosis.contains("the original failure was step"),
        "the failed step's diagnostic is preserved: {diagnosis}"
    );
    assert_eq!(
        fixture
            .runtime
            .get_task(&fixture.task_id)
            .unwrap()
            .execution_summary,
        prior_summary,
        "the refused evidence must not replace the task summary"
    );
    assert_eq!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, "recovery/prior.log")
            .unwrap()
            .expect("the prior artifact stays attached")
            .content,
        b"prior evidence\n"
    );
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, "recovery/repair.log")
            .unwrap()
            .is_none(),
        "the refused evidence must not attach its artifacts"
    );
}

/// Attaches the prior summary and artifact a refused resume must leave intact,
/// and returns the prior summary.
fn attach_prior_evidence(fixture: &Fixture) -> String {
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                upsert_artifacts: vec![orbit_types::task::TaskArtifact::from_text(
                    "recovery/prior.log",
                    "prior evidence\n",
                )],
                ..Default::default()
            },
            None,
            Some("codex".into()),
        )
        .unwrap();
    fixture
        .runtime
        .get_task(&fixture.task_id)
        .unwrap()
        .execution_summary
}

/// Runs one resume that carries repair evidence and must be refused. Asserts
/// that it escalated and that the task kept its prior summary and artifacts,
/// then returns the escalation's diagnosis.
fn refused_evidence_escalation(repair: &'static str) -> String {
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let prior_summary = attach_prior_evidence(&fixture);
    let (_, outcome) = run_pipeline(&fixture, repair);
    assert!(
        !outcome.as_ref().is_ok_and(|outcome| outcome.success),
        "{repair}: a refused resume must not succeed: {outcome:?}"
    );
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = &checkpoint.decision else {
        panic!("{repair}: a refused resume escalates: {checkpoint:?}");
    };
    assert_eq!(
        fixture
            .runtime
            .get_task(&fixture.task_id)
            .unwrap()
            .execution_summary,
        prior_summary,
        "{repair}: the refused evidence must not replace the task summary"
    );
    assert_eq!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, "recovery/prior.log")
            .unwrap()
            .expect("the prior artifact stays attached")
            .content,
        b"prior evidence\n"
    );
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, "recovery/repair.log")
            .unwrap()
            .is_none(),
        "{repair}: the refused evidence must not attach its artifacts"
    );
    diagnosis.clone()
}

/// [ORB-27959] A resume naming a step the engine will not rerun is refused
/// before its evidence is written.
fn final_recovery_refuses_evidence_for_an_inadmissible_resume_step() {
    for repair in ["evidence-admission-step", "evidence-missing-step"] {
        refused_evidence_escalation(repair);
    }
}

/// [ORB-27959] A resume whose worktree repair the commit gate refuses is
/// escalated before its evidence is written.
fn final_recovery_refuses_evidence_for_a_refused_repair_commit() {
    let diagnosis = refused_evidence_escalation("evidence-protected");
    assert!(
        diagnosis.contains("recovery_commit_refused:protected_path:"),
        "{diagnosis}"
    );
}

/// [ORB-27959] Evidence the host refuses after the resume is admitted escalates
/// with the failed step's diagnostic and writes nothing.
fn final_recovery_refuses_evidence_the_host_rejects_after_admission() {
    let diagnosis = refused_evidence_escalation("evidence-bad-artifact");
    assert!(
        diagnosis.contains("persist final recovery evidence"),
        "{diagnosis}"
    );
    assert!(
        diagnosis.contains("the original failure was step"),
        "{diagnosis}"
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
            &orbit_core::test_support::EngineHost::new(&fixture.runtime),
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
        if matches!(case, "protected" | "reviewed" | "settled-review") {
            let expected_artifact = format!("recovery-uncommitted-{run}.patch");
            assert!(
                matches!(
                    checkpoint.decision,
                    Some(FinalRecoveryDecision::Escalate { ref diagnosis, .. })
                    if diagnosis.contains(&expected_artifact)
                ),
                "{case}: diagnosis should name saved diff artifact: {checkpoint:?}"
            );
            let artifact = fixture
                .runtime
                .get_task_artifact(&fixture.task_id, &expected_artifact)
                .unwrap()
                .expect("refused final recovery saves uncommitted diff artifact");
            let text = String::from_utf8_lossy(&artifact.content);
            assert!(text.contains("repaired"), "{case}: {text}");
            if case == "protected" {
                assert!(text.contains("ordinary safe line"), "{case}: {text}");
                assert!(
                    !text.contains(".env") && !text.contains("synthetic_env_file_canary"),
                    "{case}: protected env file leaked into patch artifact: {text}"
                );
                assert!(
                    !text.contains("synthetic_patch_api_canary")
                        && !text.contains("synthetic_patch_bearer_canary")
                        && !text.contains("sk-syntheticpatchcanary00000000"),
                    "{case}: unredacted credential canary leaked into patch artifact: {text}"
                );
                assert!(
                    text.contains("[REDACTED_AUTH]") && text.contains("[REDACTED_SECRET]"),
                    "{case}: expected redaction markers in patch artifact: {text}"
                );
                let env_blob = git(&fixture, &["hash-object", ".env"]);
                let exists = crate::git_test_support::command()
                    .args(["cat-file", "-e", &env_blob])
                    .current_dir(&fixture.repo)
                    .output()
                    .unwrap()
                    .status
                    .success();
                assert!(
                    !exists,
                    "{case}: protected .env blob {env_blob} must not enter repository object store"
                );
                assert!(
                    !blob_in_object_store(&fixture, "canary.txt"),
                    "{case}: original ordinary-file bytes must not enter repository object store"
                );
            }
        }
    }
}

fn final_recovery_transition_evidence_excludes_protected_paths() {
    for case in ["transition", "scratch-transition"] {
        let mut fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
        std::fs::write(fixture.repo.join("to-dir"), "old ordinary content\n").unwrap();
        std::fs::create_dir(fixture.repo.join("to-file")).unwrap();
        std::fs::write(fixture.repo.join("to-file/.env"), "old_protected_canary\n").unwrap();
        git(&fixture, &["add", "-f", "to-dir", "to-file/.env"]);
        git(&fixture, &["commit", "-m", "seed transition"]);
        git(&fixture, &["branch", "-f", "main", "HEAD"]);
        fixture.input["base_sha"] = json!(git(&fixture, &["rev-parse", "HEAD"]));
        let (_, outcome) = run_pipeline(&fixture, case);
        assert!(outcome.is_err(), "{case}: {outcome:?}");
        let run = fixture.input["job_run_id"].as_str().unwrap();
        let name = format!("recovery-uncommitted-{run}.patch");
        let text = if case.starts_with("scratch-") {
            std::fs::read_to_string(fixture.repo.join(".orbit/tmp").join(&name)).unwrap()
        } else {
            let artifact = fixture
                .runtime
                .get_task_artifact(&fixture.task_id, &name)
                .unwrap()
                .unwrap();
            String::from_utf8(artifact.content).unwrap()
        };
        assert!(
            text.contains("repaired")
                && text.contains("new ordinary content")
                && text.contains("replacement ordinary content"),
            "{case}: {text}"
        );
        assert!(
            !text.contains(".env") && !text.contains("protected_canary"),
            "{case}: {text}"
        );
    }
}

#[cfg(unix)]
fn final_recovery_scratch_fallback_refuses_redirects() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    for redirect in [false, true] {
        let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
        let outside = tempfile::tempdir().unwrap();
        let scratch = fixture.repo.join(".orbit/tmp");
        if redirect {
            symlink(outside.path(), &scratch).unwrap();
        }
        let case = if redirect {
            "scratch-redirect"
        } else {
            "scratch-ok"
        };
        let (_, outcome) = run_pipeline(&fixture, case);
        assert!(outcome.is_err());
        let run = fixture.input["job_run_id"].as_str().unwrap();
        let checkpoint = fixture
            .runtime
            .read_run_state(run)
            .unwrap()
            .unwrap()
            .final_recovery
            .unwrap();
        let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = checkpoint.decision else {
            panic!("expected escalation")
        };
        let name = format!("recovery-uncommitted-{run}.patch");
        if redirect {
            assert!(
                diagnosis.contains("synthetic artifact refusal")
                    && diagnosis.contains("scratch write failed"),
                "{diagnosis}"
            );
            assert!(!outside.path().join(name).exists());
        } else {
            assert!(diagnosis.contains(&name), "{diagnosis}");
            let path = scratch.join(name);
            let scratch_text = std::fs::read_to_string(&path).unwrap();
            assert!(scratch_text.contains("repaired"), "{scratch_text}");
            assert!(
                scratch_text.contains("ordinary safe line"),
                "{scratch_text}"
            );
            assert!(
                !scratch_text.contains(".env")
                    && !scratch_text.contains("synthetic_env_file_canary"),
                "protected env file leaked into scratch patch: {scratch_text}"
            );
            assert!(
                !scratch_text.contains("synthetic_patch_api_canary")
                    && !scratch_text.contains("synthetic_patch_bearer_canary")
                    && !scratch_text.contains("sk-syntheticpatchcanary00000000"),
                "unredacted credential canary leaked into scratch patch: {scratch_text}"
            );
            assert!(
                scratch_text.contains("[REDACTED_AUTH]")
                    && scratch_text.contains("[REDACTED_SECRET]"),
                "expected redaction markers in scratch patch: {scratch_text}"
            );
            let env_blob = git(&fixture, &["hash-object", ".env"]);
            let exists = crate::git_test_support::command()
                .args(["cat-file", "-e", &env_blob])
                .current_dir(&fixture.repo)
                .output()
                .unwrap()
                .status
                .success();
            assert!(
                !exists,
                "protected .env blob {env_blob} must not enter repository object store"
            );
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

/// An object directory the alternates list cannot represent (its path holds
/// the path-list separator) must fail evidence capture before anything is
/// staged, not fall back to writing unredacted blobs into the repository.
#[cfg(unix)]
fn final_recovery_evidence_refuses_unisolatable_object_store() {
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let objects = fixture.repo.join(".git/objects");
    let relocated = fixture._root.path().join("objects:store");
    std::fs::rename(&objects, &relocated).unwrap();
    std::os::unix::fs::symlink(&relocated, &objects).unwrap();

    let (_, outcome) = run_pipeline(&fixture, "protected");
    assert!(outcome.is_err(), "{outcome:?}");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = checkpoint.decision else {
        panic!("expected escalation")
    };
    assert!(
        diagnosis.contains("cannot isolate scratch diff objects"),
        "capture reports the isolation failure: {diagnosis}"
    );
    let name = format!("recovery-uncommitted-{run}.patch");
    assert!(
        fixture
            .runtime
            .get_task_artifact(&fixture.task_id, &name)
            .unwrap()
            .is_none(),
        "no patch is saved without isolation"
    );
    for path in ["canary.txt", "candidate.txt", ".env"] {
        assert!(
            !blob_in_object_store(&fixture, path),
            "{path}: bytes must stay out of the repository object store"
        );
    }
    assert_eq!(
        git(&fixture, &["diff", "--cached", "--name-only"]),
        "",
        "nothing staged in the real index"
    );
}

/// Interrupt the original run after its commit and seed a generic resume from
/// its checkpoints. The reused worktree checkpoint keeps naming the original
/// batch, which therefore stays the commit step's checkpoint owner.
fn interrupted_and_resumed(fixture: &Fixture) -> (PipelineState, String, PipelineState) {
    let owner = fixture.input["job_run_id"].as_str().unwrap();
    let committed = orbit_engine::execute_deterministic_action(
        &orbit_core::test_support::EngineHost::new(&fixture.runtime),
        "git_commit",
        &json!({}),
        &fixture.input,
        false,
        &Default::default(),
        None,
    )
    .unwrap();
    assert_eq!(committed["decision"], "performed");
    let mut state = fixture.runtime.read_run_state(owner).unwrap().unwrap();
    for (index, id, output) in [
        (0, "worktree", fixture.input.clone()),
        (1, "commit", committed),
    ] {
        state.record_step(index, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output(id, output);
    }
    fixture.runtime.write_run_state(owner, &state).unwrap();
    RunLifecycle::finalize_job_run(
        &orbit_core::test_support::EngineHost::new(&fixture.runtime),
        owner,
        JobRunState::Interrupted,
        Utc::now(),
        None,
    )
    .unwrap();
    let record = orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .get_job_run(owner)
        .unwrap()
        .unwrap();
    let resumed = orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .insert_job_run(
            &record.job_id,
            2,
            Utc::now(),
            record.input.clone(),
            Some(owner.into()),
        )
        .unwrap()
        .run_id
        .to_string();
    orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .mark_job_run_running(&resumed, Utc::now(), std::process::id())
        .unwrap();
    let mut seeded = state.clone();
    seeded.run_id = resumed.clone();
    fixture.runtime.write_run_state(&resumed, &seeded).unwrap();
    (state, resumed, seeded)
}

fn commit_as(fixture: &Fixture, run: Option<&str>) -> Result<Value, OrbitError> {
    let mut input = fixture.input.clone();
    if let Some(run) = run {
        input["run_id"] = json!(run);
    }
    orbit_engine::execute_deterministic_action(
        &orbit_core::test_support::EngineHost::new(&fixture.runtime),
        "git_commit",
        &json!({}),
        &input,
        false,
        &Default::default(),
        None,
    )
    .map_err(|error| OrbitError::Execution(error.to_string()))
}

fn assert_head_refused(fixture: &Fixture, run: Option<&str>, case: &str) {
    let error = commit_as(fixture, run).unwrap_err();
    assert!(
        error.to_string().contains("worktree_head_changed"),
        "{case}: a moved HEAD needs this lineage's exact receipt: {error}"
    );
}

fn resumed_final_recovery_repair_keeps_the_original_checkpoint_owner() {
    let fixture = fixture();
    let owner = fixture.input["job_run_id"].as_str().unwrap().to_string();
    let (owner_state, resumed, seeded) = interrupted_and_resumed(&fixture);
    let host = repair_host(&fixture, "committed");
    let input = orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .get_job_run(&resumed)
        .unwrap()
        .unwrap()
        .input
        .unwrap();
    let outcome = execute_job_with_resume(
        &job(&fixture, "committed"),
        input,
        &resumed,
        audit(&fixture, &resumed),
        &host,
        Some(&seeded),
    )
    .unwrap();
    assert!(
        outcome.success,
        "ORB-16313: the resumed run's own repair receipt admits its commit: {outcome:?}"
    );
    let state = fixture.runtime.read_run_state(&resumed).unwrap().unwrap();
    let checkpoint = state.final_recovery.unwrap();
    assert_eq!(checkpoint.key.run_id, resumed);
    let repair = checkpoint.repair_commit.unwrap();
    assert_eq!(repair.head_sha, git(&fixture, &["rev-parse", "HEAD"]));
    let commits = host.commits.lock().unwrap();
    assert_eq!(
        commits.len(),
        1,
        "only the repair reruns the commit boundary"
    );
    assert_eq!(commits[0]["decision"], "already_committed");
    let validations = host.validations.lock().unwrap();
    assert_eq!(validations.len(), 2, "fresh validation for the repair head");
    assert_eq!(validations[0]["tested_head"], repair.head_sha_before);
    assert_eq!(validations[1]["tested_head"], repair.head_sha);
    let admissions = host.admissions.lock().unwrap();
    assert_eq!(
        admissions.len(),
        2,
        "fresh configured review for the repair"
    );
    assert_ne!(admissions[0]["attempt_id"], admissions[1]["attempt_id"]);
    assert_eq!(outcome.pipeline["review_gate_settle"]["gate"], "passed");
    assert_eq!(
        fixture.runtime.read_run_state(&owner).unwrap().unwrap(),
        owner_state,
        "the original owner's checkpoints stay immutable"
    );
    assert_eq!(
        fixture
            .runtime
            .get_task(&fixture.task_id)
            .unwrap()
            .job_run_id
            .as_deref(),
        Some(owner.as_str()),
        "no task binding is synthesized for the resumed run"
    );

    // The authenticated lineage accepts the exact repair; the original
    // owner's state alone, which holds no receipt, still refuses it.
    commit_as(&fixture, Some(&resumed)).unwrap();
    assert_head_refused(&fixture, None, "original owner without a receipt");
    let receipt = fixture.runtime.read_run_state(&resumed).unwrap().unwrap();
    let record = orbit_core::test_support::EngineHost::new(&fixture.runtime)
        .get_job_run(&owner)
        .unwrap()
        .unwrap();
    for (case, job_id, source) in [
        ("unrelated run", record.job_id.as_str(), None),
        ("other job lineage", "other_job", Some(owner.clone())),
    ] {
        let copy = orbit_core::test_support::EngineHost::new(&fixture.runtime)
            .insert_job_run(job_id, 2, Utc::now(), record.input.clone(), source)
            .unwrap()
            .run_id
            .to_string();
        let mut state = receipt.clone();
        state.run_id = copy.clone();
        fixture.runtime.write_run_state(&copy, &state).unwrap();
        assert_head_refused(&fixture, Some(&copy), case);
    }
    for case in ["head", "before", "workspace", "key", "decision"] {
        let mut state = receipt.clone();
        let checkpoint = state.final_recovery.as_mut().unwrap();
        let repair = checkpoint.repair_commit.as_mut().unwrap();
        match case {
            "head" => repair.head_sha = repair.head_sha_before.clone(),
            "before" => repair.head_sha_before = "0".repeat(40),
            "workspace" => repair.workspace_path = fixture.repo.join(".orbit"),
            "key" => checkpoint.key.run_id = "jrun-foreign".into(),
            "decision" => {
                checkpoint.outcome = Some(FinalRecoveryCheckpointOutcome::Escalated {
                    detail: "refused".into(),
                })
            }
            _ => unreachable!(),
        }
        fixture.runtime.write_run_state(&resumed, &state).unwrap();
        assert_head_refused(&fixture, Some(&resumed), case);
    }
    fixture.runtime.write_run_state(&resumed, &receipt).unwrap();
    for rebound in [resumed.as_str(), "jrun-foreign"] {
        fixture
            .runtime
            .update_task_with_identity(
                &fixture.task_id,
                orbit_core::application::task::TaskUpdateParams {
                    job_run_id: Some(Some(rebound.into())),
                    ..Default::default()
                },
                Some("codex".into()),
                None,
            )
            .unwrap();
        // The batch no longer owns the task, so the commit refuses before
        // any receipt can be consulted.
        let error = commit_as(&fixture, Some(&resumed)).unwrap_err();
        assert!(
            error.to_string().contains("expected exactly one task"),
            "a task rebound away from the owner gains no commit authority: {error}"
        );
    }
    fixture
        .runtime
        .update_task_with_identity(
            &fixture.task_id,
            orbit_core::application::task::TaskUpdateParams {
                job_run_id: Some(Some(owner.clone())),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    commit_as(&fixture, Some(&resumed)).unwrap();
    git(
        &fixture,
        &["commit", "--allow-empty", "-m", "unrecorded descendant"],
    );
    assert_head_refused(&fixture, Some(&resumed), "descendant of the repair");
}

fn final_recovery_refused_patch_reproduces_binary_and_final_whitespace() {
    let mut fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    std::fs::write(fixture.repo.join("tracked.bin"), b"\x00\x01\x02\x03\xff\n").unwrap();
    std::fs::write(fixture.repo.join("raw.dat"), b"hdr\xfe\xfd\n").unwrap();
    std::fs::write(
        fixture.repo.join("trailing.txt"),
        b"first line\nsecond line\n",
    )
    .unwrap();
    std::fs::write(fixture.repo.join("no_nl.txt"), b"seed\n").unwrap();
    git(
        &fixture,
        &["add", "tracked.bin", "raw.dat", "trailing.txt", "no_nl.txt"],
    );
    git(&fixture, &["commit", "-m", "seed binary and text"]);
    git(&fixture, &["branch", "-f", "main", "HEAD"]);
    fixture.input["base_sha"] = json!(git(&fixture, &["rev-parse", "HEAD"]));

    let (host, outcome) = run_pipeline(&fixture, "reviewed-binary");
    assert!(outcome.is_err(), "{outcome:?}");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let name = format!("recovery-uncommitted-{run}.patch");
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &name)
        .unwrap()
        .expect("refused final recovery saves patch artifact");
    let candidate_head = host.validation_heads.lock().unwrap()[0].0.clone();

    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let clone = crate::git_test_support::command()
        .args([
            "clone",
            "-q",
            fixture.repo.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone.status.success(), "{clone:?}");
    let co = crate::git_test_support::command()
        .args(["checkout", "-q", &candidate_head])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(co.status.success(), "{co:?}");
    let patch_file = root.path().join("recovery.patch");
    std::fs::write(&patch_file, &artifact.content).unwrap();
    let apply = crate::git_test_support::command()
        .args(["apply", patch_file.to_str().unwrap()])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "git apply failed ({}):\n{}",
        apply.status,
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        std::fs::read(checkout.join("candidate.txt")).unwrap(),
        b"repaired\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("tracked.bin")).unwrap(),
        b"\x00\x89PNG\r\n\x1a\n\x00\xff\xfe\x04"
    );
    assert_eq!(
        std::fs::read(checkout.join("raw.dat")).unwrap(),
        b"hdr\x80\x81\xff\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("trailing.txt")).unwrap(),
        b"first line\nfinal spaces   \n"
    );
    assert_eq!(
        std::fs::read(checkout.join("no_nl.txt")).unwrap(),
        b"no newline   "
    );
    assert_eq!(
        std::fs::read(checkout.join("added.bin")).unwrap(),
        b"\x7fELF\x00\x01\x02\xff"
    );
}

#[cfg(unix)]
fn final_recovery_refused_patch_preserves_non_utf8_paths() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let tracked_fffd = "mod-\u{FFFD}.pyc";
    std::fs::write(
        fixture.repo.join(tracked_fffd),
        b"tracked replacement fixture\n",
    )
    .unwrap();
    git(&fixture, &["add", tracked_fffd]);
    git(&fixture, &["commit", "-m", "seed replacement-char file"]);
    git(&fixture, &["branch", "-f", "main", "HEAD"]);
    fixture.input["base_sha"] = json!(git(&fixture, &["rev-parse", "HEAD"]));

    let (host, outcome) = run_pipeline(&fixture, "non-utf8");
    assert!(outcome.is_err(), "{outcome:?}");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = checkpoint.decision else {
        panic!("expected escalation")
    };
    let name = format!("recovery-uncommitted-{run}.patch");
    assert!(
        diagnosis.contains("recovery_commit_refused:uncommittable:")
            && diagnosis.contains("not valid UTF-8")
            && diagnosis.contains(&name),
        "refusal and saved patch artifact expected: {diagnosis}"
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &name)
        .unwrap()
        .expect("refused non-UTF-8 candidate saves uncommitted diff artifact");
    let text = String::from_utf8_lossy(&artifact.content);
    assert!(
        !text.contains(".env")
            && !text.contains("non_utf8_dir_secret")
            && !text.contains("top_secret"),
        "protected env files must not leak into patch artifact: {text}"
    );
    for protected_rel in [OsStr::from_bytes(b"dir-\xfe/.env"), OsStr::new(".env")] {
        let blob = crate::git_test_support::command()
            .args([OsStr::new("hash-object"), protected_rel])
            .current_dir(&fixture.repo)
            .output()
            .unwrap();
        assert!(blob.status.success());
        let sha = String::from_utf8(blob.stdout).unwrap();
        let exists = crate::git_test_support::command()
            .args(["cat-file", "-e", sha.trim()])
            .current_dir(&fixture.repo)
            .output()
            .unwrap()
            .status
            .success();
        assert!(
            !exists,
            "protected blob must not enter repository object store"
        );
    }

    let candidate_head = host.validation_heads.lock().unwrap()[0].0.clone();
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let clone = crate::git_test_support::command()
        .args([
            "clone",
            "-q",
            fixture.repo.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone.status.success(), "{clone:?}");
    let co = crate::git_test_support::command()
        .args(["checkout", "-q", &candidate_head])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(co.status.success(), "{co:?}");
    let patch_file = root.path().join("recovery.patch");
    std::fs::write(&patch_file, &artifact.content).unwrap();
    let apply = crate::git_test_support::command()
        .args(["apply", patch_file.to_str().unwrap()])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "git apply failed ({}):\n{}",
        apply.status,
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        std::fs::read(checkout.join("candidate.txt")).unwrap(),
        b"repaired\n"
    );
    assert_eq!(
        std::fs::read(checkout.join(OsStr::from_bytes(b"loose-\xff.pyc"))).unwrap(),
        b"\x00nonutf8\xff"
    );
    assert_eq!(
        std::fs::read(checkout.join(OsStr::from_bytes(b"dir-\xfe/sub-\xfd.txt"))).unwrap(),
        b"native path content\n"
    );
    assert_eq!(
        std::fs::read(checkout.join(tracked_fffd)).unwrap(),
        b"tracked replacement fixture\n"
    );
    assert!(!checkout.join(OsStr::from_bytes(b"dir-\xfe/.env")).exists());
    assert!(!checkout.join(".env").exists());
}

#[cfg(unix)]
fn final_recovery_refused_patch_reproduces_worktree_rename() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let mut fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let native_source = OsStr::from_bytes(b"rename-src-\xfe.txt");
    std::fs::write(fixture.repo.join(native_source), b"moved native content\n").unwrap();
    let add = crate::git_test_support::command()
        .args([OsStr::new("add"), native_source])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(add.status.success(), "{add:?}");
    git(&fixture, &["commit", "-m", "seed native rename source"]);
    git(&fixture, &["branch", "-f", "main", "HEAD"]);
    fixture.input["base_sha"] = json!(git(&fixture, &["rev-parse", "HEAD"]));

    let (host, outcome) = run_pipeline(&fixture, "worktree-rename");
    assert!(outcome.is_err(), "{outcome:?}");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = checkpoint.decision else {
        panic!("expected escalation")
    };
    let name = format!("recovery-uncommitted-{run}.patch");
    assert!(
        diagnosis.contains("recovery_commit_refused:uncommittable:")
            && diagnosis.contains("not valid UTF-8")
            && diagnosis.contains(&name),
        "refusal and saved patch artifact expected: {diagnosis}"
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &name)
        .unwrap()
        .expect("refused rename candidate saves uncommitted diff artifact");

    let candidate_head = host.validation_heads.lock().unwrap()[0].0.clone();
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let clone = crate::git_test_support::command()
        .args([
            "clone",
            "-q",
            fixture.repo.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone.status.success(), "{clone:?}");
    let co = crate::git_test_support::command()
        .args(["checkout", "-q", &candidate_head])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(co.status.success(), "{co:?}");
    assert!(checkout.join(native_source).exists());
    let patch_file = root.path().join("recovery.patch");
    std::fs::write(&patch_file, &artifact.content).unwrap();
    let apply = crate::git_test_support::command()
        .args(["apply", patch_file.to_str().unwrap()])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "git apply failed ({}):\n{}",
        apply.status,
        String::from_utf8_lossy(&apply.stderr)
    );
    assert!(
        !checkout.join(native_source).exists(),
        "the rename's native source must be deleted: {}",
        String::from_utf8_lossy(&artifact.content)
    );
    assert_eq!(
        std::fs::read(checkout.join("renamed.txt")).unwrap(),
        b"moved native content\n"
    );
    assert_eq!(
        std::fs::read(checkout.join("candidate.txt")).unwrap(),
        b"repaired\n"
    );
}

fn final_recovery_refused_patch_preserves_staged_ignored_addition() {
    let mut fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    std::fs::write(
        fixture.repo.join(".gitignore"),
        "*.ignored\nignored-dir/\n.env.*\n",
    )
    .unwrap();
    git(&fixture, &["add", ".gitignore"]);
    git(&fixture, &["commit", "-m", "seed gitignore"]);
    git(&fixture, &["branch", "-f", "main", "HEAD"]);
    fixture.input["base_sha"] = json!(git(&fixture, &["rev-parse", "HEAD"]));

    let (host, outcome) = run_pipeline(&fixture, "staged-ignored");
    assert!(outcome.is_err(), "{outcome:?}");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    assert!(checkpoint.repair_commit.is_none());
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = checkpoint.decision else {
        panic!("expected escalation")
    };
    let name = format!("recovery-uncommitted-{run}.patch");
    assert!(
        diagnosis.contains("recovery_commit_refused:protected_path:")
            && diagnosis.contains(&name)
            && !diagnosis.contains("failed to capture uncommitted diff"),
        "refusal and saved patch artifact expected: {diagnosis}"
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &name)
        .unwrap()
        .expect("refused candidate with staged ignored addition saves patch artifact");
    let text = String::from_utf8_lossy(&artifact.content);
    assert!(
        text.contains("repaired")
            && text.contains("notes.ignored")
            && text.contains("staged ignored content")
            && text.contains("ignored-dir/nested.txt")
            && text.contains("nested ignored content")
            && text.contains("deleted.ignored")
            && text.contains("gone before capture"),
        "independent safe edit and staged ignored additions (including removed-from-disk AD) must be preserved: {text}"
    );
    assert!(
        !text.contains("sk-syntheticignoredcanary0000") && text.contains("[REDACTED_SECRET]"),
        "credential canary inside staged ignored addition must be redacted: {text}"
    );
    assert!(
        !text.contains(".env")
            && !text.contains("synthetic_ignored_env_canary")
            && !text.contains("synthetic_force_added_env_canary")
            && !text.contains("untracked.txt")
            && !text.contains("must not stage"),
        "protected and unstaged-ignored paths must stay out of patch: {text}"
    );
    for path in [".env", "notes.ignored", "candidate.txt"] {
        assert!(
            !blob_in_object_store(&fixture, path),
            "{path}: bytes must stay out of the repository object store"
        );
    }
    assert_eq!(
        git(&fixture, &["diff", "--cached", "--name-only"]),
        ".env.local\ndeleted.ignored\nignored-dir/nested.txt\nnotes.ignored",
        "live index must remain untouched"
    );

    let candidate_head = host.validation_heads.lock().unwrap()[0].0.clone();
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let clone = crate::git_test_support::command()
        .args([
            "clone",
            "-q",
            fixture.repo.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone.status.success(), "{clone:?}");
    let co = crate::git_test_support::command()
        .args(["checkout", "-q", &candidate_head])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(co.status.success(), "{co:?}");
    let patch_file = root.path().join("recovery.patch");
    std::fs::write(&patch_file, &artifact.content).unwrap();
    let apply = crate::git_test_support::command()
        .args(["apply", patch_file.to_str().unwrap()])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "git apply failed ({}):\n{}",
        apply.status,
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("candidate.txt")).unwrap(),
        "repaired\n"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("notes.ignored")).unwrap(),
        "staged ignored content\n[REDACTED_SECRET]\n"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("ignored-dir/nested.txt")).unwrap(),
        "nested ignored content\n"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("deleted.ignored")).unwrap(),
        "gone before capture\n"
    );
    assert!(!checkout.join("ignored-dir/untracked.txt").exists());
    assert!(!checkout.join(".env").exists());
    assert!(!checkout.join(".env.local").exists());
}

fn final_recovery_refused_patch_preserves_staged_only_work() {
    // 1. Only staged-only safe work exists (`MM` reverted to HEAD on disk, plus
    // `AD` newly staged file removed from disk) alongside a protected `.env`.
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let (host, outcome) = run_pipeline(&fixture, "staged-only");
    assert!(outcome.is_err(), "{outcome:?}");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = checkpoint.decision else {
        panic!("expected escalation")
    };
    let name = format!("recovery-uncommitted-{run}.patch");
    assert!(
        diagnosis.contains("recovery_commit_refused:protected_path:") && diagnosis.contains(&name),
        "staged-only refused repair must attach uncommitted diff artifact: {diagnosis}"
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &name)
        .unwrap()
        .expect("refused repair with only live-index work saves uncommitted diff artifact");
    let text = String::from_utf8_lossy(&artifact.content);
    assert!(
        text.contains("staged repair") && text.contains("staged new file"),
        "staged-only tracked edit and deleted addition must appear in patch: {text}"
    );
    assert!(
        !text.contains("sk-syntheticstagedcanary0000") && text.contains("[REDACTED_SECRET]"),
        "secret canary in staged-only edit must be redacted: {text}"
    );
    assert!(
        !text.contains(".env") && !text.contains("synthetic_staged_only_env_canary"),
        "protected env file must stay out of patch: {text}"
    );
    assert!(!blob_in_object_store(&fixture, ".env"));
    assert_eq!(
        git(&fixture, &["diff", "--cached", "--name-only"]),
        "candidate.txt\nstaged_deleted.txt",
        "live index must remain untouched"
    );

    let candidate_head = host.validation_heads.lock().unwrap()[0].0.clone();
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let clone = crate::git_test_support::command()
        .args([
            "clone",
            "-q",
            fixture.repo.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone.status.success(), "{clone:?}");
    let co = crate::git_test_support::command()
        .args(["checkout", "-q", &candidate_head])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(co.status.success(), "{co:?}");
    let patch_file = root.path().join("staged_only.patch");
    std::fs::write(&patch_file, &artifact.content).unwrap();
    let apply = crate::git_test_support::command()
        .args(["apply", patch_file.to_str().unwrap()])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "git apply failed ({}):\n{}",
        apply.status,
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("candidate.txt")).unwrap(),
        "staged repair\n[REDACTED_SECRET]\n"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("staged_deleted.txt")).unwrap(),
        "staged new file\n"
    );
    assert!(!checkout.join(".env").exists());

    // 2. Mixed case: worktree edit + staged-only edits + path with distinct
    // staged and worktree edits (`both_changed.txt`).
    let mut mixed = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    std::fs::write(mixed.repo.join("tracked_reverted.txt"), "base\n").unwrap();
    std::fs::write(mixed.repo.join("both_changed.txt"), "base\n").unwrap();
    git(&mixed, &["add", "tracked_reverted.txt", "both_changed.txt"]);
    git(&mixed, &["commit", "-m", "seed staged-mixed files"]);
    git(&mixed, &["branch", "-f", "main", "HEAD"]);
    mixed.input["base_sha"] = json!(git(&mixed, &["rev-parse", "HEAD"]));

    let (mixed_host, mixed_outcome) = run_pipeline(&mixed, "staged-mixed");
    assert!(mixed_outcome.is_err(), "{mixed_outcome:?}");
    let mixed_run = mixed.input["job_run_id"].as_str().unwrap();
    let mixed_name = format!("recovery-uncommitted-{mixed_run}.patch");
    let mixed_artifact = mixed
        .runtime
        .get_task_artifact(&mixed.task_id, &mixed_name)
        .unwrap()
        .expect("mixed refused repair saves uncommitted diff artifact");
    let mixed_text = String::from_utf8_lossy(&mixed_artifact.content);
    assert!(
        mixed_text.contains("repaired")
            && mixed_text.contains("staged reverted edit")
            && mixed_text.contains("staged deleted new")
            && mixed_text.contains("worktree final"),
        "worktree and staged-only work must appear in mixed patch: {mixed_text}"
    );
    assert!(
        mixed_text.contains("retained live index") && mixed_text.contains("both_changed.txt"),
        "distinct staged state superseded by worktree edit must be expressly identified: {mixed_text}"
    );
    assert!(
        !mixed_text.contains(".env") && !mixed_text.contains("synthetic_staged_mixed_env_canary"),
        "protected env file must stay out of mixed patch: {mixed_text}"
    );

    let mixed_head = mixed_host.validation_heads.lock().unwrap()[0].0.clone();
    let mixed_checkout = root.path().join("mixed_checkout");
    let clone2 = crate::git_test_support::command()
        .args([
            "clone",
            "-q",
            mixed.repo.to_str().unwrap(),
            mixed_checkout.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone2.status.success(), "{clone2:?}");
    let co2 = crate::git_test_support::command()
        .args(["checkout", "-q", &mixed_head])
        .current_dir(&mixed_checkout)
        .output()
        .unwrap();
    assert!(co2.status.success(), "{co2:?}");
    let mixed_patch = root.path().join("mixed.patch");
    std::fs::write(&mixed_patch, &mixed_artifact.content).unwrap();
    let apply2 = crate::git_test_support::command()
        .args(["apply", mixed_patch.to_str().unwrap()])
        .current_dir(&mixed_checkout)
        .output()
        .unwrap();
    assert!(
        apply2.status.success(),
        "git apply failed on mixed patch ({}):\n{}",
        apply2.status,
        String::from_utf8_lossy(&apply2.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(mixed_checkout.join("candidate.txt")).unwrap(),
        "repaired\n"
    );
    assert_eq!(
        std::fs::read_to_string(mixed_checkout.join("tracked_reverted.txt")).unwrap(),
        "staged reverted edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(mixed_checkout.join("staged_deleted.txt")).unwrap(),
        "staged deleted new\n"
    );
    assert_eq!(
        std::fs::read_to_string(mixed_checkout.join("both_changed.txt")).unwrap(),
        "worktree final\n"
    );
}

fn final_recovery_refused_patch_omits_staged_path_under_replaced_dir() {
    // A staged addition whose parent directory was replaced on disk by a
    // regular file (`AD replaced-dir/nested.txt` + `?? replaced-dir`) must
    // not abort the capture: the absent staged path is omitted while the
    // unrelated safe edit and the replacement file are preserved.
    let fixture = fixture_with_command("test \"$(cat candidate.txt)\" != after");
    let (host, outcome) = run_pipeline(&fixture, "staged-replaced-dir");
    assert!(outcome.is_err(), "{outcome:?}");
    let run = fixture.input["job_run_id"].as_str().unwrap();
    let checkpoint = fixture
        .runtime
        .read_run_state(run)
        .unwrap()
        .unwrap()
        .final_recovery
        .unwrap();
    assert!(checkpoint.repair_commit.is_none());
    let Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) = checkpoint.decision else {
        panic!("expected escalation")
    };
    let name = format!("recovery-uncommitted-{run}.patch");
    assert!(
        diagnosis.contains("recovery_commit_refused:protected_path:")
            && diagnosis.contains(&name)
            && !diagnosis.contains("failed to capture uncommitted diff")
            && !diagnosis.contains("failed to inspect uncommitted path"),
        "refusal must name the saved patch, not an inspection failure: {diagnosis}"
    );
    let artifact = fixture
        .runtime
        .get_task_artifact(&fixture.task_id, &name)
        .unwrap()
        .expect("refused repair with a replaced parent directory saves patch artifact");
    let text = String::from_utf8_lossy(&artifact.content);
    assert!(
        text.contains("repaired") && text.contains("replacement file content"),
        "independent safe edit and replacement file must be preserved: {text}"
    );
    assert!(
        !text.contains("nested staged content"),
        "absent staged path under the replaced directory must be omitted: {text}"
    );
    assert!(
        !text.contains(".env") && !text.contains("synthetic_replaced_dir_env_canary"),
        "protected env file must stay out of patch: {text}"
    );
    assert!(!blob_in_object_store(&fixture, ".env"));
    assert_eq!(
        git(&fixture, &["diff", "--cached", "--name-only"]),
        "replaced-dir/nested.txt",
        "live index must remain untouched"
    );

    let candidate_head = host.validation_heads.lock().unwrap()[0].0.clone();
    let root = tempfile::tempdir().unwrap();
    let checkout = root.path().join("checkout");
    let clone = crate::git_test_support::command()
        .args([
            "clone",
            "-q",
            fixture.repo.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(clone.status.success(), "{clone:?}");
    let co = crate::git_test_support::command()
        .args(["checkout", "-q", &candidate_head])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(co.status.success(), "{co:?}");
    let patch_file = root.path().join("replaced_dir.patch");
    std::fs::write(&patch_file, &artifact.content).unwrap();
    let apply = crate::git_test_support::command()
        .args(["apply", patch_file.to_str().unwrap()])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        apply.status.success(),
        "git apply failed ({}):\n{}",
        apply.status,
        String::from_utf8_lossy(&apply.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("candidate.txt")).unwrap(),
        "repaired\n"
    );
    assert_eq!(
        std::fs::read_to_string(checkout.join("replaced-dir")).unwrap(),
        "replacement file content\n"
    );
    assert!(!checkout.join("replaced-dir/nested.txt").exists());
    assert!(!checkout.join(".env").exists());
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    final_recovery_repair_commit_gets_a_fresh_before_pr_review_scenarios,
    final_recovery_repair_commit_gets_a_fresh_before_pr_review,
    final_recovery_commits_a_landing_review_repair_with_unsettled_admission,
    final_recovery_does_not_authorize_an_unrecorded_head_change,
    final_recovery_persists_repair_evidence_without_task_write_tools,
    final_recovery_refuses_unattributed_repair_evidence_before_persisting,
    final_recovery_refuses_evidence_for_an_inadmissible_resume_step,
    final_recovery_refuses_evidence_for_a_refused_repair_commit,
    final_recovery_refuses_evidence_the_host_rejects_after_admission,
    final_recovery_transition_evidence_excludes_protected_paths,
    resumed_final_recovery_repair_keeps_the_original_checkpoint_owner,
    final_recovery_refused_patch_reproduces_binary_and_final_whitespace,
    final_recovery_refused_patch_preserves_staged_ignored_addition,
    final_recovery_refused_patch_preserves_staged_only_work,
    final_recovery_refused_patch_omits_staged_path_under_replaced_dir
);

#[cfg(unix)]
crate::scenario_table!(
    scratch_fallback_scenarios,
    final_recovery_scratch_fallback_refuses_redirects,
    final_recovery_evidence_refuses_unisolatable_object_store,
    final_recovery_refused_patch_preserves_non_utf8_paths,
    final_recovery_refused_patch_reproduces_worktree_rename
);
