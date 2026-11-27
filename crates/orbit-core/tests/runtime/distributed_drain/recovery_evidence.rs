//! A recovery host carries evidence to its claim owner without task-write tools.

use orbit_engine::test_support::impl_test_host;
use orbit_engine::{HostEnvironment, Recovery, RunLifecycle};
use std::sync::atomic::{AtomicBool, Ordering};

use super::claimed_review::ToOwner;
use super::*;
use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, TaskAutomationUpdate, V2AuditWriter, execute_job_with_resume,
};
use orbit_types::tool::WorkerInvocation;
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::{FinalRecoveryCheckpoint, FinalRecoveryKey};

#[test]
fn final_recovery_handoff_persists_only_its_claimed_tasks_evidence() {
    if !isolated(
        module_path!(),
        "final_recovery_handoff_persists_only_its_claimed_tasks_evidence",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let other = pair.tasks.iter().find(|id| **id != task).unwrap();
    let record = pair.admission(&leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    let bound = pair
        .follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: task.clone(),
                claim_id: claim.claim_id,
                execution: claim.executed_on,
                bound_run_id: leaf.clone(),
            },
            Arc::new(ToOwner(pair.wire.owner.clone())),
        )
        .unwrap();
    let mut state = PipelineState::new(leaf.clone(), "claimed_task_pipeline".into(), json!({}));
    state.final_recovery = Some(FinalRecoveryCheckpoint {
        fault: Default::default(),
        key: FinalRecoveryKey {
            run_id: leaf.clone(),
            attempt: 1,
        },
        failed_step_id: "implement".into(),
        task_id: task.clone(),
        observed: None,
        repair_commit: None,
        resume_context: None,
        base_ref: Some("main".into()),
        admitted_at: Utc::now(),
        decision: None,
        outcome: None,
    });
    pair.follower.write_run_state(&leaf, &state).unwrap();
    let scratch = pair.follower_repo.join(".orbit/tmp");
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::write(scratch.join("repair.log"), b"verified claimed repair\n").unwrap();
    let evidence = json!({"execution_summary": "Outcome: success\nChanges: repaired claimed candidate.",
        "task_artifacts": [{"path": "recovery/repair.log", "source_path": ".orbit/tmp/repair.log"}]});
    let persist = |id: &str| {
        Recovery::persist_final_recovery_evidence(
            &orbit_core::test_support::EngineHost::new(&bound),
            &leaf,
            id,
            &pair.follower_repo,
            &evidence,
        )
    };
    assert!(
        persist(other).is_err(),
        "the recovery cannot write another owner's task"
    );
    persist(&task).expect("host sends bounded evidence through the authenticated claim route");
    let owner = &pair.wire.owner;
    assert_eq!(
        owner.get_task(&task).unwrap().execution_summary,
        "Outcome: success\nChanges: repaired claimed candidate."
    );
    assert_eq!(
        owner
            .get_task_artifact(&task, "recovery/repair.log")
            .unwrap()
            .unwrap()
            .content,
        b"verified claimed repair\n"
    );
    assert!(
        owner
            .get_task_artifact(other, "recovery/repair.log")
            .unwrap()
            .is_none()
    );
    assert!(owner.get_task(other).unwrap().execution_summary.is_empty());
    // A replay after recovery decides must not reopen its evidence authority.
    state.final_recovery.as_mut().unwrap().decision =
        Some(orbit_types::workflow::FinalRecoveryDecision::Resume {
            step_id: "implement".into(),
            rationale: "repair complete".into(),
        });
    pair.follower.write_run_state(&leaf, &state).unwrap();
    assert!(
        persist(&task).is_err(),
        "decided recovery has no evidence-write authority"
    );
}

struct RecordingBoundary {
    inner: ToOwner,
    refuse_artifact_put: AtomicBool,
    calls: Mutex<Vec<(String, Value)>>,
}

impl OwnerCoordinator for RecordingBoundary {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .unwrap()
            .push((name.into(), input.clone()));
        if name == "orbit.task.artifact.put" && self.refuse_artifact_put.load(Ordering::SeqCst) {
            return Err(OrbitError::PolicyDenied(
                "simulated owner artifact transport refusal".into(),
            ));
        }
        self.inner.call(name, input, session)
    }
}

struct ClaimedRecoveryHost {
    bound: OrbitRuntime,
    worktree_output: Value,
}

impl_test_host! {
    impl Dispatch for ClaimedRecoveryHost {
        fn run_deterministic(
            &self,
            action: &str,
            config: &Value,
            input: &Value,
            context: ToolContext,
        ) -> Result<Value, DispatchError> {
            match action {
                "setup" => Ok(self.worktree_output.clone()),
                "fail_validate" => Err(DispatchError::DeterministicActionFailed {
                    action: "claim_validate".into(),
                    message: "candidate check failed".into(),
                }),
                "decide" => {
                    let repo = Path::new(self.worktree_output["workspace_path"].as_str().unwrap());
                    std::fs::write(repo.join("candidate.txt"), "repaired\nTOKEN=x\n").unwrap();
                std::fs::write(repo.join(".env"), "PROTECTED_ENV_CANARY=x\n").unwrap();
                Ok(json!({
                    "decision": "resume",
                    "fault": "implementer",
                    "fault_evidence": "fixture: validation failed",
                    "step_id": "validate",
                    "rationale": "repaired candidate",
                }))
            }
            _ => orbit_core::test_support::EngineHost::new(&self.bound)
                .run_deterministic(action, config, input, context),
        }
    }
    }
}

impl_test_host! {
    impl HostEnvironment for ClaimedRecoveryHost {
        fn required_validation_commands(&self) -> Vec<String> {
            orbit_core::test_support::EngineHost::new(&self.bound).required_validation_commands()
        }

        fn run_settings(&self) -> orbit_engine::RunSettings {
            HostEnvironment::run_settings(&orbit_core::test_support::EngineHost::new(&self.bound))
        }
    }
}

impl_test_host! {
    impl RunLifecycle for ClaimedRecoveryHost {
        fn list_run_tasks(&self, run: &str) -> Result<Vec<orbit_types::task::Task>, OrbitError> {
            self.bound.list_run_tasks(run)
        }

        fn get_task(&self, task: &str) -> Result<orbit_types::task::Task, OrbitError> {
            self.bound.get_task(task)
        }

        fn apply_task_automation_update(
            &self,
            task_id: &str,
            update: TaskAutomationUpdate,
        ) -> Result<(), OrbitError> {
            RunLifecycle::apply_task_automation_update(
                &orbit_core::test_support::EngineHost::new(&self.bound),
                task_id,
                update,
            )
        }

        fn read_run_state(&self, run: &str) -> Result<Option<PipelineState>, OrbitError> {
            self.bound.read_run_state(run)
        }

        fn get_job_run(
            &self,
            run: &str,
        ) -> Result<Option<orbit_types::workflow::JobRun>, OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.bound).get_job_run(run)
        }
    }
}

impl_test_host! {
    impl Recovery for ClaimedRecoveryHost {
        fn checkpoint_step(
            &self,
            run_id: &str,
            index: u32,
            step_id: &str,
            output: &Value,
            compound: &BTreeMap<String, Value>,
            script_outputs: &BTreeMap<String, orbit_types::workflow::activity_job::ScriptOutputSet>,
        ) -> Result<(), DispatchError> {
            orbit_core::test_support::EngineHost::new(&self.bound).checkpoint_step(
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
            workspace: &Path,
            evidence: &Value,
        ) -> Result<(), OrbitError> {
            Recovery::persist_final_recovery_evidence(
                &orbit_core::test_support::EngineHost::new(&self.bound),
                run_id,
                task_id,
                workspace,
                evidence,
            )
        }

        fn final_recovery_log_tail(&self, run: &str) -> Result<Option<String>, OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.bound).final_recovery_log_tail(run)
        }

        fn admit_final_recovery(
            &self,
            run: &str,
            request: &FinalRecoveryAdmissionRequest,
        ) -> Result<FinalRecoveryAdmission, OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.bound)
                .admit_final_recovery(run, request)
        }

        fn apply_final_recovery(
            &self,
            run: &str,
            application: &FinalRecoveryApplication,
        ) -> Result<FinalRecoveryApplied, OrbitError> {
            Recovery::apply_final_recovery(
                &orbit_core::test_support::EngineHost::new(&self.bound),
                run,
                application,
            )
        }
    }
}

impl_test_host! {
    impl ReviewGate for ClaimedRecoveryHost {
        fn attach_task_validation_log(
            &self,
            task: &str,
            run: &str,
            path: &str,
            content: Vec<u8>,
        ) -> Result<(), OrbitError> {
            orbit_core::test_support::EngineHost::new(&self.bound)
                .attach_task_validation_log(task, run, path, content)
        }
    }
}

impl_test_host! {
    impl RuntimeHostProcessEnvironment for ClaimedRecoveryHost {}
}

struct ClaimedRecoveryFixture {
    pair: Pair,
    drain: String,
    leaf: String,
    task: String,
    other: String,
    boundary: Arc<RecordingBoundary>,
    bound: OrbitRuntime,
    base_sha: String,
    head_sha: String,
}

fn setup_claimed_recovery_fixture(refuse_artifact_put: bool) -> ClaimedRecoveryFixture {
    let pair = Pair::with_configs(
        "",
        "[workflow]\nfinal_recovery_crews = [\"sol\"]\n",
        &[None, None],
    );
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    let other = pair.tasks.iter().find(|id| **id != task).unwrap().clone();
    let repo = pair.follower_repo.clone();
    std::fs::write(repo.join(".gitignore"), "/.orbit/\n").unwrap();
    std::fs::write(repo.join("candidate.txt"), "before\n").unwrap();
    let git = |args: &[&str]| -> String {
        let output = crate::git_test_support::run(&repo, args).unwrap();
        assert!(output.success, "git {args:?}: {}", output.stderr);
        output.stdout.trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "fixture"]);
    git(&["config", "user.email", "fixture@orbit.invalid"]);
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "baseline"]);
    let base_sha = git(&["rev-parse", "HEAD"]);
    git(&["checkout", "-q", "-b", &format!("orbit/{task}")]);
    std::fs::write(repo.join("candidate.txt"), "after\n").unwrap();
    git(&["commit", "-q", "-am", "implement candidate"]);
    let head_sha = git(&["rev-parse", "HEAD"]);

    let record = pair.admission(&leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    let boundary = Arc::new(RecordingBoundary {
        inner: ToOwner(pair.wire.owner.clone()),
        refuse_artifact_put: AtomicBool::new(refuse_artifact_put),
        calls: Mutex::new(Vec::new()),
    });
    let bound = pair
        .follower
        .clone()
        .with_worker_invocation(
            WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: task.clone(),
                claim_id: claim.claim_id,
                execution: claim.executed_on,
                bound_run_id: leaf.clone(),
            },
            boundary.clone(),
        )
        .unwrap();
    ClaimedRecoveryFixture {
        pair,
        drain,
        leaf,
        task,
        other,
        boundary,
        bound,
        base_sha,
        head_sha,
    }
}

fn recovery_job(worktree_output: &Value) -> JobV2 {
    let step = |id: &str, action: &str| {
        json!({
            "id": id,
            "default_input": worktree_output,
            "spec": {"type": "deterministic", "action": action, "config": {}},
        })
    };
    let mut job = load_job_asset(
        &json!({
            "schemaVersion": 2,
            "kind": "Job",
            "metadata": {"name": "claimed_recovery_refusal"},
            "spec": {
                "state": "enabled",
                "kind": "workflow",
                "steps": [
                    step("worktree", "setup"),
                    step("commit", "git_commit"),
                    step("validate", "fail_validate"),
                ],
            },
        })
        .to_string(),
    )
    .unwrap()
    .spec;
    job.final_recovery_activity = Some("decide".into());
    job.resolved_final_recovery_activity = Some(
        load_activity_asset(
            &json!({
                "schemaVersion": 2,
                "kind": "Activity",
                "metadata": {"name": "decide"},
                "spec": {
                    "type": "deterministic",
                    "description": "Repair failing candidate",
                    "action": "decide",
                    "config": {},
                },
            })
            .to_string(),
        )
        .unwrap()
        .spec,
    );
    job
}

fn run_refused_recovery_commit(fixture: &ClaimedRecoveryFixture, audit_root: &Path) -> String {
    let worktree_output = json!({
        "task_ids": [fixture.task],
        "workspace_path": fixture.pair.follower_repo,
        "base_ref": "main",
        "base_sha": fixture.base_sha,
        "job_run_id": fixture.leaf,
    });
    let job = recovery_job(&worktree_output);
    let input = json!({"task_ids": [fixture.task], "claimed": true});
    let mut state = PipelineState::new(
        fixture.leaf.clone(),
        "claimed_recovery_refusal".into(),
        input.clone(),
    );
    state.record_step(0, JobRunState::Success, Some(worktree_output.clone()), None);
    state.record_step(
        1,
        JobRunState::Success,
        Some(json!({
            "phase": "commit",
            "task_id": fixture.task,
            "head_sha": fixture.head_sha,
        })),
        None,
    );
    fixture
        .pair
        .follower
        .write_run_state(&fixture.leaf, &state)
        .unwrap();
    let audit = V2AuditWriter::with_disk_sinks(
        audit_root,
        orbit_store::Store::open_in_memory().unwrap(),
        "ws_fixture",
        &fixture.leaf,
        "fixture",
        None,
    )
    .unwrap();
    let host = ClaimedRecoveryHost {
        bound: fixture.bound.clone(),
        worktree_output,
    };
    let outcome = execute_job_with_resume(&job, input, &fixture.leaf, audit, &host, Some(&state));
    assert!(
        outcome.is_err(),
        "refused recovery commit must fail: {outcome:?}"
    );
    let checkpoint = fixture
        .bound
        .read_run_state(&fixture.leaf)
        .unwrap()
        .unwrap()
        .final_recovery
        .expect("final recovery checkpoint");
    match checkpoint.decision {
        Some(FinalRecoveryDecision::Escalate { diagnosis, .. }) => diagnosis,
        other => panic!("expected Escalate decision after refused commit, got {other:?}"),
    }
}

#[test]
fn claimed_final_recovery_refused_commit_routes_patch_to_owner_or_fallback() {
    if !isolated(
        module_path!(),
        "claimed_final_recovery_refused_commit_routes_patch_to_owner_or_fallback",
    ) {
        return;
    }

    // 1. Accepted artifact transport: owner stores the patch artifact before
    // the diagnostic names `task artifact`, and no fallback file is created.
    let accepted = setup_claimed_recovery_fixture(false);
    let artifact_name = format!("recovery-uncommitted-{}.patch", accepted.leaf);
    let audit_dir = tempfile::tempdir().unwrap();
    let diagnosis = run_refused_recovery_commit(&accepted, audit_dir.path());
    assert!(
        diagnosis.contains("recovery_commit_refused:protected_path:"),
        "{diagnosis}"
    );
    assert!(
        diagnosis.contains(&format!(
            "uncommitted diff saved to task artifact `{artifact_name}`"
        )),
        "accepted transport should name owner task artifact: {diagnosis}"
    );
    let owner = &accepted.pair.wire.owner;
    let stored = owner
        .get_task_artifact(&accepted.task, &artifact_name)
        .unwrap()
        .expect("owner must hold the refused recovery patch artifact");
    let patch_text = String::from_utf8_lossy(&stored.content);
    assert!(patch_text.contains("repaired"), "{patch_text}");
    assert!(patch_text.contains("TOKEN=x"), "{patch_text}");
    let retrieved = accepted
        .bound
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": accepted.task, "path": artifact_name}),
        )
        .expect("bound worker can retrieve matching artifact from owner");
    assert_eq!(retrieved["encoding"], "base64", "{retrieved}");
    let decoded = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        retrieved["content_base64"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(decoded, stored.content);
    let owner_retrieved = owner
        .run_tool(
            "orbit.task.artifact.get",
            json!({"id": accepted.task, "path": artifact_name}),
        )
        .expect("owner can retrieve matching artifact via tool");
    assert_eq!(
        owner_retrieved["content_base64"],
        retrieved["content_base64"]
    );
    assert!(
        owner
            .get_task_artifact(&accepted.other, &artifact_name)
            .unwrap()
            .is_none(),
        "unrelated task must not receive the claimed task's recovery artifact"
    );
    assert!(
        !accepted
            .pair
            .follower_repo
            .join(".orbit/tmp")
            .join(&artifact_name)
            .exists(),
        "accepted owner transport should not write the scratch fallback"
    );
    let boundary_calls = accepted.boundary.calls.lock().unwrap().clone();
    assert!(
        boundary_calls
            .iter()
            .any(|(name, _)| name == "orbit.task.artifact.put"),
        "expected artifact transport call across owner boundary: {boundary_calls:?}"
    );

    // Claim fence preservation on the active claim:
    // - Cannot write an artifact to another task.
    let foreign_err = RunLifecycle::apply_task_automation_update(
        &orbit_core::test_support::EngineHost::new(&accepted.bound),
        &accepted.other,
        TaskAutomationUpdate {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "recovery-uncommitted-foreign.patch",
                "foreign diff",
            )],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        foreign_err
            .to_string()
            .contains("worker task binding mismatch"),
        "expected task binding refusal: {foreign_err}"
    );
    assert!(
        owner
            .get_task_artifact(&accepted.other, "recovery-uncommitted-foreign.patch")
            .unwrap()
            .is_none()
    );
    // - Direct artifact payload inside `_worker_update` stays rejected by owner.
    let record = accepted.pair.admission(&accepted.leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    let direct_err = owner
        .execute_owner_coordination(
            "orbit.task.update",
            json!({
                "id": accepted.task,
                "_worker_update": orbit_store::contract::ClaimWorkerUpdate {
                    evidence: ClaimEvidence {
                        artifacts: vec![TaskArtifact::from_text("direct.patch", "diff")],
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }),
            ToolSessionContext {
                worker_invocation: Some(WorkerInvocation {
                    owner_machine_id: OWNER.into(),
                    owner_workspace_id: record.destination.owner_workspace_id,
                    owner_destination: record.destination.selector,
                    task_id: accepted.task.clone(),
                    claim_id: claim.claim_id,
                    execution: claim.executed_on,
                    bound_run_id: accepted.leaf.clone(),
                }),
                worker_host_call: true,
                effective_capabilities: BTreeSet::from([McpCapability::Agent]),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        direct_err
            .to_string()
            .contains("use the task artifact adapter for artifact payloads"),
        "direct artifact payload inside _worker_update must remain rejected: {direct_err}"
    );
    // - Once the claim is no longer active, artifact writes are refused as stale_claim.
    accepted.pair.leaf_fails_with(&accepted.leaf, "claim ended");
    accepted.pair.pass(&accepted.drain);
    let stale_err = RunLifecycle::apply_task_automation_update(
        &orbit_core::test_support::EngineHost::new(&accepted.bound),
        &accepted.task,
        TaskAutomationUpdate {
            upsert_artifacts: vec![TaskArtifact::from_text(
                "recovery-uncommitted-stale.patch",
                "stale diff",
            )],
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        stale_err.to_string().contains("stale_claim"),
        "inactive claim must refuse artifact write: {stale_err}"
    );
    assert!(
        owner
            .get_task_artifact(&accepted.task, "recovery-uncommitted-stale.patch")
            .unwrap()
            .is_none()
    );

    // 2. Refused artifact transport: worker writes the `.orbit/tmp/` fallback
    // and names it in the diagnostic; owner holds no artifact.
    let refused = setup_claimed_recovery_fixture(true);
    let refused_artifact = format!("recovery-uncommitted-{}.patch", refused.leaf);
    let refused_audit_dir = tempfile::tempdir().unwrap();
    let refused_diagnosis = run_refused_recovery_commit(&refused, refused_audit_dir.path());
    assert!(
        refused_diagnosis.contains("recovery_commit_refused:protected_path:"),
        "{refused_diagnosis}"
    );
    assert!(
        refused_diagnosis.contains(&format!(
            "uncommitted diff saved to `.orbit/tmp/{refused_artifact}` (task artifact write failed:"
        )),
        "refused transport should name fallback path: {refused_diagnosis}"
    );
    assert!(
        !refused_diagnosis.contains("saved to task artifact"),
        "refused transport must not claim owner task artifact was saved: {refused_diagnosis}"
    );
    let fallback_path = refused
        .pair
        .follower_repo
        .join(".orbit/tmp")
        .join(&refused_artifact);
    let fallback_text = std::fs::read_to_string(&fallback_path)
        .expect("refused transport must write documented scratch fallback");
    assert!(fallback_text.contains("repaired"), "{fallback_text}");
    assert!(fallback_text.contains("TOKEN=x"), "{fallback_text}");
    assert!(
        refused
            .pair
            .wire
            .owner
            .get_task_artifact(&refused.task, &refused_artifact)
            .unwrap()
            .is_none(),
        "refused transport must leave owner without artifact"
    );

    // 3. Refused artifact transport AND failed fallback: diagnostic must not
    // name any saved patch location.
    let double_fail = setup_claimed_recovery_fixture(true);
    let double_artifact = format!("recovery-uncommitted-{}.patch", double_fail.leaf);
    let double_audit_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(
        double_fail
            .pair
            .follower_repo
            .join(".orbit/tmp")
            .join(&double_artifact),
    )
    .expect("block scratch fallback path with directory");
    let double_diagnosis = run_refused_recovery_commit(&double_fail, double_audit_dir.path());
    assert!(
        double_diagnosis.contains("failed to capture uncommitted diff:"),
        "{double_diagnosis}"
    );
    assert!(
        !double_diagnosis.contains("uncommitted diff saved to"),
        "failed fallback must not name a saved patch: {double_diagnosis}"
    );
    assert!(
        double_fail
            .pair
            .wire
            .owner
            .get_task_artifact(&double_fail.task, &double_artifact)
            .unwrap()
            .is_none()
    );
}
