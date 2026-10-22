//! A published host candidate survives a metadata-only implementation replay.
use super::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use orbit_engine::activity_job::{load_activity_asset, load_job_asset};
use orbit_engine::{
    ClaimExecutionContext, DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication, FinalRecoveryApplied, RuntimeHost, V2AuditWriter,
    execute_job_with_resume,
};
use orbit_types::workflow::activity_job::JobV2;
use orbit_types::workflow::handoff::TaskHandoff;
use orbit_types::workflow::{
    FinalRecoveryCheckpoint, FinalRecoveryKey, JobRunState, PipelineState,
};

const TASK_ID: &str = "ORB-1";

struct ReplayHost<'a> {
    fx: &'a Fixture,
    state: Mutex<PipelineState>,
    implementations: Mutex<usize>,
    commits: Mutex<Vec<Value>>,
    handoffs: Mutex<Vec<TaskHandoff>>,
    published: Mutex<Option<String>>,
    mode: &'static str,
    validations: Mutex<Vec<String>>,
    refused: Mutex<Vec<String>>,
    syncs: Mutex<Vec<Value>>,
    interrupted: Mutex<bool>,
}

impl orbit_engine::test_support::NullRuntimeHost for ReplayHost<'_> {
    fn final_recovery_log_tail(&self, _: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    fn get_task(&self, id: &str) -> Result<Task, OrbitError> {
        assert_eq!(id, TASK_ID);
        let mut task = RuntimeHost::get_task(&Host::new(&self.fx.repo), super::TASK_ID)?;
        task.id = TASK_ID.into();
        Ok(task)
    }

    fn list_run_tasks(&self, _: &str) -> Result<Vec<Task>, OrbitError> {
        Ok(vec![RuntimeHost::get_task(self, TASK_ID)?])
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(path_str(&self.fx.repo).into())
    }

    fn read_run_state(&self, run: &str) -> Result<Option<PipelineState>, OrbitError> {
        assert_eq!(run, RUN_ID);
        Ok(Some(self.state.lock().unwrap().clone()))
    }

    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Ok(ClaimExecutionContext {
            footprint: vec!["dir:src".into()],
            workspace_id: "ws-test".into(),
            task_id: TASK_ID.into(),
            claim_id: "claim-replay".into(),
            machine_id: "machine-test".into(),
            run_id: RUN_ID.into(),
            ship_mode: "pr".into(),
            base_branch: BASE.into(),
            landing_branch: BASE.into(),
            required_commands: vec![],
        })
    }

    fn record_claim_handoff(&self, handoff: &TaskHandoff) -> Result<(), OrbitError> {
        self.handoffs.lock().unwrap().push(handoff.clone());
        Ok(())
    }

    fn run_deterministic(
        &self,
        name: &str,
        config: &Value,
        input: &Value,
        _: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        match name {
            "setup" => {
                return Ok(json!({
                    "workspace_path": self.fx.claim, "job_run_id": RUN_ID,
                    "base_sha": self.fx.base_sha, "base_ref": BASE, "head_ref": OURS,
                }));
            }
            "implement" => {
                let mut count = self.implementations.lock().unwrap();
                *count += 1;
                if *count == 1 {
                    fs::create_dir_all(self.fx.claim.join("src")).unwrap();
                    fs::write(self.fx.claim.join("src/feature.txt"), "candidate\n").unwrap();
                    self.fx.advance_base();
                } else {
                    assert!((2..=3).contains(&*count));
                    assert!(input["final_recovery_resume"].is_object());
                    assert!(git(&self.fx.claim, &["status", "--porcelain"]).is_empty());
                    if *count == 3 {
                        assert_eq!(
                            git(&self.fx.claim, &["rev-parse", "HEAD"]),
                            self.state.lock().unwrap().pipeline["commit"]["commit_sha"]
                        );
                    } else {
                        assert_eq!(
                            git(&self.fx.claim, &["rev-parse", "HEAD"]),
                            *self.published.lock().unwrap().as_ref().unwrap()
                        );
                    }
                    match self.mode {
                        "files" | "restart-files" => fs::write(
                            self.fx.claim.join("src/feature.txt"),
                            "repaired candidate\n",
                        )
                        .unwrap(),
                        "provider-head" => {
                            git(
                                &self.fx.claim,
                                &["commit", "--allow-empty", "-qm", "provider commit"],
                            );
                        }
                        "rewind" => {
                            let committed =
                                self.state.lock().unwrap().pipeline["commit"]["commit_sha"]
                                    .as_str()
                                    .unwrap()
                                    .to_string();
                            git(&self.fx.claim, &["reset", "--hard", &committed]);
                        }
                        "branch" => {
                            git(&self.fx.claim, &["checkout", "-qb", "foreign"]);
                        }
                        "stale" => {
                            let mut state = self.state.lock().unwrap();
                            // Internally consistent but for the wrong candidate.
                            let old = state.pipeline["commit"]["commit_sha"].clone();
                            state.pipeline["sync_base"]["head_sha"] = old.clone();
                            let index = state
                                .step_outputs
                                .iter()
                                .find(|(_, output)| output.get("head_sha_before").is_some())
                                .map(|(index, _)| *index)
                                .unwrap();
                            state.step_outputs.get_mut(&index).unwrap()["head_sha"] = old;
                        }
                        "run" => self.state.lock().unwrap().run_id = "jrun-foreign".into(),
                        "task" => {
                            let mut state = self.state.lock().unwrap();
                            state.final_recovery.as_mut().unwrap().task_id = "ORB-99".into();
                        }
                        "missing" => {
                            self.state.lock().unwrap().step_states.remove(&2);
                        }
                        _ => {}
                    }
                    if matches!(
                        self.mode,
                        "provider-head"
                            | "rewind"
                            | "branch"
                            | "stale"
                            | "run"
                            | "task"
                            | "missing"
                    ) {
                        fs::write(self.fx.claim.join("unstaged.txt"), "must not be staged\n")
                            .unwrap();
                    }
                }
                return Ok(
                    json!({"execution_summary": "Outcome: success\nChanges: implemented candidate.",
                    "regression_from": [if *count == 1 { TASK_ID } else { "ORB-2" }]}),
                );
            }
            "checkpoint_pause" => {
                let mut interrupted = self.interrupted.lock().unwrap();
                if matches!(self.mode, "restart-commit" | "restart-files")
                    && self.commits.lock().unwrap().len() == 2
                    && !*interrupted
                {
                    *interrupted = true;
                    return Err(DispatchError::DeterministicActionFailed {
                        action: name.into(),
                        message: "fixture interruption after replay commit".into(),
                    });
                }
                return Ok(json!({"continued": true}));
            }
            "decide" => {
                self.refused
                    .lock()
                    .unwrap()
                    .push(input["error_message"].as_str().unwrap().into());
                assert_eq!(
                    input["failed_step_id"], "handoff",
                    "unexpected original error: {input}"
                );
                return Ok(json!({"decision": "resume", "step_id": "implement_bundle",
                    "rationale": "Correct self-attribution; preserve the published candidate"}));
            }
            _ => {}
        }
        if name == "git_push" {
            git_fixture::assert_local_push_remote(&self.fx.repo);
        }
        let output =
            execute_deterministic_action(self, name, config, input, false, &HashMap::new(), None)
                .map_err(|e| DispatchError::DeterministicActionFailed {
                action: name.into(),
                message: e.to_string(),
            })?;
        Ok(output)
    }

    fn checkpoint_step(
        &self,
        _: &str,
        index: u32,
        id: &str,
        output: &Value,
        compound: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        if id == "pr_open" {
            let head = git(&self.fx.claim, &["rev-parse", "HEAD"]);
            let mut published = self.published.lock().unwrap();
            if !matches!(self.mode, "files" | "restart-files")
                && let Some(previous) = &*published
            {
                assert_eq!(&head, previous);
            }
            *published = Some(head);
        }
        if id == "commit" {
            self.commits.lock().unwrap().push(output.clone());
        }
        if id == "sync_base" {
            self.syncs.lock().unwrap().push(output.clone());
        }
        if id == "validate" {
            self.validations.lock().unwrap().push(
                output["candidate"]["candidate"]["commit"]
                    .as_str()
                    .unwrap()
                    .into(),
            );
        }
        let mut state = self.state.lock().unwrap();
        state.record_step(index, JobRunState::Success, Some(output.clone()), None);
        state.record_compound_outputs(index, compound.clone());
        state.record_pipeline_output(id, output.clone());
        for (key, output) in compound {
            state.record_pipeline_output(key, output.clone());
        }
        Ok(())
    }

    fn admit_final_recovery(
        &self,
        _: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        let mut state = self.state.lock().unwrap();
        assert!(state.final_recovery.is_none());
        state.final_recovery = Some(FinalRecoveryCheckpoint {
            key: FinalRecoveryKey {
                run_id: RUN_ID.into(),
                attempt: 1,
            },
            failed_step_id: request.failed_step_id.clone(),
            task_id: TASK_ID.into(),
            observed: None,
            base_ref: Some(BASE.into()),
            admitted_at: Utc::now(),
            decision: None,
            repair_commit: None,
            resume_context: None,
            outcome: None,
        });
        Ok(FinalRecoveryAdmission::Admitted)
    }

    fn apply_final_recovery(
        &self,
        _: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        let mut state = self.state.lock().unwrap();
        let checkpoint = state.final_recovery.as_mut().unwrap();
        checkpoint.decision = Some(application.decision.clone());
        checkpoint.outcome = Some("resume".into());
        checkpoint.repair_commit = application.repair_commit.clone();
        checkpoint.resume_context = application.resume_context.clone();
        let from = application
            .resume_step_index
            .unwrap_or_else(|| panic!("recovery not admitted: {application:?}"));
        state
            .step_states
            .retain(|index, _| *index < from || application.preserved_steps.contains(index));
        state
            .step_outputs
            .retain(|index, _| *index < from || application.preserved_steps.contains(index));
        state.compound_outputs.retain(|index, _| *index < from);
        if self.mode == "restart" {
            Err(OrbitError::Execution(
                "fixture interruption after durable resume".into(),
            ))
        } else {
            Ok(FinalRecoveryApplied::Resume)
        }
    }
}

fn job(fx: &Fixture) -> JobV2 {
    let common = json!({"job_run_id": RUN_ID, "run_id": RUN_ID, "task_ids": [TASK_ID],
        "completed_task_ids": [TASK_ID], "workspace_path": fx.claim, "base": BASE,
        "base_ref": BASE, "base_sync": "local", "base_sha": fx.base_sha,
        "implementation": "{{ steps.implement_one.output }}"});
    let step = |id: &str, action: &str, extra: Value| {
        let mut input = common.clone();
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        json!({"id": id, "spec": {"type": "deterministic", "action": action, "config": {}},
            "default_input": input})
    };
    let asset = json!({"schemaVersion": 2, "kind": "Job", "metadata": {"name": "claimed_replay"},
    "spec": {"state": "enabled", "kind": "workflow", "steps": [
        step("worktree", "setup", json!({"implementation": null})),
        {"id": "implement_bundle", "loop": {"items": "{{ input.task_ids }}", "max_iterations": 1,
            "steps": [{"id": "implement_one", "spec": {"type": "deterministic", "action": "implement", "config": {}},
                "default_input": {"workspace_path": fx.claim}}]}},
        step("commit", "git_commit", json!({})),
        step("checkpoint_pause", "checkpoint_pause", json!({})),
        step("prepare_branch", "pr_prepare", json!({})),
        step("sync_base", "git_rebase", json!({
            "head": "{{ steps.prepare_branch.output.head }}", "head_sha": "{{ steps.prepare_branch.output.head_sha }}",
            "base_sha": "{{ steps.prepare_branch.output.base_sha }}", "remote_sha": "{{ steps.prepare_branch.output.remote_sha }}",
            "commits_behind": "{{ steps.prepare_branch.output.commits_behind }}",
            "sync_required": "{{ steps.prepare_branch.output.sync_required }}"})),
        step("push", "git_push", json!({"branch": OURS, "rewrite_performed": "{{ steps.sync_base.output.rewritten }}"})),
        step("pr_open", "pr_open", json!({"head": OURS, "base_sha": "{{ steps.sync_base.output.base_sha }}"})),
        step("validate", "claim_validate", json!({"pull_request": "{{ steps.pr_open.output.pr_number }}"})),
        step("handoff", "claim_handoff", json!({"pull_request": "{{ steps.pr_open.output.pr_number }}",
            "candidate": "{{ steps.validate.output.candidate }}", "validation": "{{ steps.validate.output.validation }}"})),
    ]}});
    let mut job = load_job_asset(&asset.to_string()).unwrap().spec;
    job.final_recovery_activity = Some("decide".into());
    job.resolved_final_recovery_activity = Some(load_activity_asset(&json!({
        "schemaVersion": 2, "kind": "Activity", "metadata": {"name": "decide"},
        "spec": {"type": "deterministic", "description": "Repair metadata", "action": "decide", "config": {}}
    }).to_string()).unwrap().spec);
    job
}

fn scenario(sandbox: &Path, mode: &'static str) {
    let fx = Fixture::new(sandbox);
    let host = ReplayHost {
        fx: &fx,
        state: Mutex::new(PipelineState::new(
            RUN_ID.into(),
            "claimed_replay".into(),
            json!({"task_ids": [TASK_ID]}),
        )),
        implementations: Mutex::new(0),
        commits: Mutex::new(vec![]),
        handoffs: Mutex::new(vec![]),
        published: Mutex::new(None),
        mode,
        validations: Mutex::new(vec![]),
        refused: Mutex::new(vec![]),
        syncs: Mutex::new(vec![]),
        interrupted: Mutex::new(false),
    };
    let sink = Arc::new(orbit_engine::V2SqliteSink::for_audit_root(
        Arc::new(orbit_store::Store::open_in_memory().unwrap()),
        "ws-test",
        RUN_ID,
        "test-agent",
        None,
        sandbox,
    ));
    let audit = Arc::new(
        V2AuditWriter::new(
            RUN_ID,
            "test-agent",
            Arc::new(orbit_agent::loop_engine::InMemorySink::new(
                sandbox.join("audit"),
            )),
        )
        .with_envelope_sink(sink),
    );
    let job = job(&fx);
    let input = json!({"task_ids": [TASK_ID], "claimed": true});
    let mut outcome =
        execute_job_with_resume(&job, input.clone(), RUN_ID, audit.clone(), &host, None);
    if matches!(mode, "restart" | "restart-commit" | "restart-files") {
        assert!(
            outcome.is_err(),
            "the fixture interrupts after persisting resume"
        );
        let resume = host.state.lock().unwrap().clone();
        outcome = execute_job_with_resume(&job, input, RUN_ID, audit, &host, Some(&resume));
    }
    assert!(
        host.refused.lock().unwrap()[0].contains("own regression culprit"),
        "the original public handoff refuses self-attribution"
    );
    let syncs = host.syncs.lock().unwrap();
    assert_ne!(
        syncs[0]["head_sha_before"], syncs[0]["head_sha"],
        "the host rebased the first candidate before publication"
    );
    let refused = matches!(
        mode,
        "provider-head" | "rewind" | "branch" | "stale" | "run" | "task" | "missing"
    );
    if refused {
        let error = outcome.expect_err("foreign or stale candidates must be refused");
        assert!(
            error.to_string().contains("worktree_head_changed"),
            "{mode}: {error}"
        );
        assert!(
            git(&fx.claim, &["diff", "--cached", "--name-only"]).is_empty(),
            "refusal never stages the sentinel"
        );
        assert!(fx.claim.join("unstaged.txt").exists());
        assert!(host.handoffs.lock().unwrap().is_empty());
        assert_eq!(host.commits.lock().unwrap().len(), 1);
        assert_eq!(
            git(&fx.claim, &["rev-parse", "--abbrev-ref", "HEAD"]),
            if mode == "branch" { "foreign" } else { OURS }
        );
        let head = git(&fx.claim, &["rev-parse", "HEAD"]);
        let published = host.published.lock().unwrap();
        if mode == "provider-head" {
            assert_ne!(&head, published.as_ref().unwrap());
            assert_eq!(
                git(&fx.claim, &["rev-parse", "HEAD^"]),
                *published.as_ref().unwrap(),
                "provider history is left untouched"
            );
        } else if mode == "rewind" {
            assert_eq!(
                head,
                host.commits.lock().unwrap()[0]["commit_sha"],
                "the refusal leaves the rewind untouched"
            );
        } else {
            assert_eq!(&head, published.as_ref().unwrap());
        }
        return;
    }
    assert!(
        outcome.as_ref().is_ok_and(|o| o.success),
        "published candidate must continue: {outcome:?}"
    );
    let committed = host.commits.lock().unwrap();
    assert_eq!(
        committed.len(),
        if matches!(mode, "restart-commit" | "restart-files") {
            3
        } else {
            2
        }
    );
    assert_eq!(committed[0]["decision"], "performed");
    assert_eq!(
        committed[1]["decision"],
        if matches!(mode, "files" | "restart-files") {
            "performed"
        } else {
            "already_committed"
        }
    );
    let validations = host.validations.lock().unwrap();
    assert_eq!(
        validations.len(),
        2,
        "both passes validate their own candidate"
    );
    assert_eq!(validations[0], syncs[0]["head_sha"]);
    assert_eq!(
        &validations[1],
        host.published.lock().unwrap().as_ref().unwrap()
    );
    if matches!(mode, "files" | "restart-files") {
        assert_ne!(validations[0], validations[1]);
    } else {
        assert_eq!(validations[0], validations[1]);
    }
    let handoffs = host.handoffs.lock().unwrap();
    assert_eq!(handoffs.len(), 1);
    assert_eq!(handoffs[0].regression_from, ["ORB-2"]);
    assert_eq!(handoffs[0].candidate.candidate.commit, validations[1]);
    assert_eq!(
        fx.calls()
            .iter()
            .filter(|call| *call == "pr create")
            .count(),
        1
    );
    assert_eq!(
        git(&fx.claim, &["rev-list", "--count", "main..HEAD"]),
        if matches!(mode, "files" | "restart-files") {
            "2"
        } else {
            "1"
        }
    );
    assert!(git(&fx.claim, &["status", "--porcelain"]).is_empty());
}

#[test]
fn published_candidate_survives_metadata_replay() {
    isolated(
        "final_recovery::published_candidate_survives_metadata_replay",
        |sandbox| scenario(sandbox, "metadata"),
    );
}

#[test]
fn published_replay_revalidates_file_repairs_and_survives_interruption() {
    isolated(
        "final_recovery::published_replay_revalidates_file_repairs_and_survives_interruption",
        |sandbox| {
            scenario(sandbox, "files");
            scenario(sandbox, "restart");
            scenario(sandbox, "restart-commit");
            scenario(sandbox, "restart-files");
        },
    );
}

#[test]
fn published_replay_refuses_foreign_history_and_stale_evidence() {
    isolated(
        "final_recovery::published_replay_refuses_foreign_history_and_stale_evidence",
        |sandbox| {
            for mode in [
                "provider-head",
                "rewind",
                "branch",
                "stale",
                "run",
                "task",
                "missing",
            ] {
                scenario(sandbox, mode);
            }
        },
    );
}
