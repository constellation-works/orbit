#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(missing_docs)]

//! [ORB-13907] The runtime's side of the engine's final recovery hook,
//! through the `RuntimeHost` methods the engine calls: admission is recorded
//! in the run's state once, an empty crew pool skips it, a `resume` drops the
//! step checkpoints it makes stale, and any other decision reaches the task
//! through the applier — once, even when a crash or a failed run-state write
//! interrupts the settlement.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_engine::{
    DispatchError, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, RuntimeHost,
};
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::{
    FinalRecoveryCheckpointOutcome, FinalRecoveryDecision, JobRunState, PipelineState,
};
use serde_json::{Value, json};
use tempfile::TempDir;

mod recorded_decision;
mod repair_commit;
mod revision;
mod terminalization;

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    runtime: OrbitRuntime,
    repo: PathBuf,
    jobs: Arc<dyn JobRunStoreBackend>,
}

fn fixture(pool: &str) -> Fixture {
    let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(
        repo.join(".orbit/config.toml"),
        format!(
            "[workflow]\ndefault_crew = \"sol\"\nfinal_recovery_crews = {pool}\n\n\
             [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n\n\
             [crews.luna]\nprovider = \"codex\"\nmodel = \"luna-model\"\n"
        ),
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &repo.join(".orbit")).unwrap();
    let jobs = orbit_store::compose::workspace_job_run_store(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
    );
    Fixture {
        _root: root,
        global,
        runtime,
        repo,
        jobs,
    }
}

impl Fixture {
    /// An in-progress task, as a failing run holds it.
    fn task(&self) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Recover a failed run",
                    "description": "Final recovery fixture task.",
                    "acceptance_criteria": ["Recovered."],
                    "complexity": "low",
                    "workspace": self.repo.to_string_lossy(),
                    "type": "chore",
                    "model": "codex"
                }),
            )
            .unwrap();
        let id = task["id"].as_str().unwrap().to_string();
        for update in [
            json!({"id": id, "plan": "1. Do it.", "model": "codex"}),
            json!({"id": id, "status": "backlog", "model": "codex"}),
            json!({"id": id, "status": "in_progress", "model": "codex"}),
        ] {
            self.runtime.run_tool("orbit.task.update", update).unwrap();
        }
        id
    }

    /// A task whose implementer needs tools a recovery activity may disallow.
    fn task_requiring(&self, tools: &[&str]) -> String {
        let task = self
            .runtime
            .run_tool(
                "orbit.task.add",
                json!({
                    "title": "Require a tool recovery must not call",
                    "description": "Admission fixture for a disallowed requirement.",
                    "acceptance_criteria": ["Admitted."],
                    "complexity": "low",
                    "workspace": self.repo.to_string_lossy(),
                    "type": "chore",
                    "model": "codex",
                    "required_tools": tools,
                }),
            )
            .unwrap();
        task["id"].as_str().unwrap().to_string()
    }

    /// A running pipeline whose first three steps have checkpoints.
    fn running_run(&self, task: &str) -> String {
        let run = self.pipeline_run(task, "task_pr_pipeline");
        let mut state = self.state(&run);
        for index in 0..3 {
            state.record_step(
                index,
                JobRunState::Success,
                Some(json!({ "step": index })),
                None,
            );
        }
        self.jobs.write_run_state(&run, &state).unwrap();
        run
    }

    fn pipeline_run(&self, task: &str, job: &str) -> String {
        let input = json!({ "task_ids": [task] });
        let run = self
            .jobs
            .insert_job_run(job, 1, Utc::now(), Some(input.clone()), None)
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let state = PipelineState::new(run.run_id.clone(), run.job_id, input);
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    fn state(&self, run: &str) -> PipelineState {
        self.jobs.read_run_state(run).unwrap().unwrap()
    }

    fn admit(&self, run: &str, task: &str) -> FinalRecoveryAdmission {
        self.runtime
            .admit_final_recovery(
                run,
                &FinalRecoveryAdmissionRequest {
                    task_id: task.to_string(),
                    failed_step_id: "implement".to_string(),
                    base_ref: Some("main".to_string()),
                },
            )
            .unwrap()
    }

    fn apply(
        &self,
        run: &str,
        task: &str,
        decision: FinalRecoveryDecision,
        resume_step_index: Option<u32>,
    ) -> FinalRecoveryApplied {
        self.apply_result(run, task, decision, resume_step_index)
            .unwrap()
    }

    fn apply_result(
        &self,
        run: &str,
        task: &str,
        decision: FinalRecoveryDecision,
        resume_step_index: Option<u32>,
    ) -> Result<FinalRecoveryApplied, orbit_core::OrbitError> {
        // Named through the trait: `OrbitRuntime` also has the applier's own
        // `apply_final_recovery`, which the host method calls after recording.
        RuntimeHost::apply_final_recovery(
            &self.runtime,
            run,
            &FinalRecoveryApplication {
                fault: Default::default(),
                task_id: task.to_string(),
                failed_step_id: "implement".to_string(),
                decision,
                resume_step_index,
                preserved_steps: Vec::new(),
                repair_commit: None,
                resume_context: None,
                workspace_path: self.repo.clone(),
                completion_done: false,
                infrastructure: None,
            },
        )
    }

    /// Fail every run-state write that would record a final-recovery
    /// outcome, as a crash or a full disk would between the task write and
    /// the run's own record. Writes that record only the decision still land.
    /// Dropping the returned guard restores the store.
    fn fail_outcome_writes(&self) -> OutcomeWriteFault {
        let db = orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::new(
            &self.global,
            self.repo.join(".orbit"),
        ))
        .unwrap();
        let connection = rusqlite::Connection::open(db).unwrap();
        connection
            .execute_batch(
                "CREATE TRIGGER fail_recovery_outcome \
                 BEFORE UPDATE ON job_run_states \
                 WHEN NEW.pipeline_state_json LIKE '%\"final_recovery\"%\"outcome\"%' \
                 BEGIN SELECT RAISE(ABORT, 'injected run-state write failure'); END; \
                 CREATE TRIGGER fail_recovery_outcome_insert \
                 BEFORE INSERT ON job_run_states \
                 WHEN NEW.pipeline_state_json LIKE '%\"final_recovery\"%\"outcome\"%' \
                 BEGIN SELECT RAISE(ABORT, 'injected run-state write failure'); END;",
            )
            .unwrap();
        OutcomeWriteFault(connection)
    }

    /// A run resumed from `source`: a new run seeded with a clone of its
    /// state, as `orbit run resume` seeds one.
    fn resumed_run(&self, source: &str) -> String {
        let input = json!({ "task_ids": [] });
        let run = self
            .jobs
            .insert_job_run(
                "task_pr_pipeline",
                2,
                Utc::now(),
                Some(input),
                Some(source.to_string()),
            )
            .unwrap();
        self.jobs
            .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
            .unwrap();
        let mut state = self.state(source);
        state.run_id = run.run_id.clone();
        self.jobs.write_run_state(&run.run_id, &state).unwrap();
        run.run_id
    }

    /// The decision comments the applier wrote on `task`.
    fn decision_comments(&self, task: &str) -> usize {
        self.runtime
            .run_tool("orbit.task.show", json!({ "id": task }))
            .unwrap()["comments"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|comment| {
                comment["message"]
                    .as_str()
                    .is_some_and(|message| message.starts_with("final_recovery run_id="))
            })
            .count()
    }

    fn status(&self, task: &str) -> String {
        self.runtime
            .run_tool("orbit.task.show", json!({ "id": task }))
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    /// A task blocked by a stopped delivery run, plus the distinct
    /// blocked-task recovery run that would apply a backstop decision to it.
    /// With `publish`, the failed run pushed and opened its pull request
    /// before it stopped. Returns `(task, publisher, recovery, episode)`.
    fn blocked_backstop_episode(&self, publish: bool) -> (String, String, String, Value) {
        let task = self.task();
        let publisher = self.pipeline_run(&task, "task_pr_pipeline");
        if publish {
            let mut state = self.state(&publisher);
            state.record_pipeline_output("push", json!({ "branch": "orbit/backstop-fixture" }));
            state.record_pipeline_output("pr_open", json!({ "pr_number": "17" }));
            self.jobs.write_run_state(&publisher, &state).unwrap();
        }
        let mut run = self.runtime.show_job_run_observed(&publisher).unwrap();
        run.state = JobRunState::Failed;
        run.pid = None;
        run.finished_at = Some(Utc::now());
        self.runtime
            .sqlite_store()
            .unwrap()
            .upsert_job_run_for_workspace(&self.runtime.workspace_id().unwrap(), &run, None)
            .unwrap();
        RuntimeHost::apply_task_automation_update(
            &self.runtime,
            &task,
            orbit_engine::blocked_workflow_failure_update(
                "task_pr_pipeline",
                &publisher,
                Some("STEP_FAILED"),
                Some("delivery failed after publication"),
            ),
        )
        .unwrap();
        assert_eq!(self.status(&task), "blocked");
        let view = self
            .runtime
            .blocked_recovery_view(Utc::now())
            .unwrap()
            .into_iter()
            .find(|view| view.episode.task_id == task)
            .expect("blocked episode");
        let observed = self
            .runtime
            .final_recovery_revision(&self.runtime.get_task(&task).unwrap())
            .unwrap();
        let episode = orbit_core::application::task::BlockedRecoveryInput {
            task_id: task.clone(),
            episode_key: view.episode.key(),
            block_source: "run_failed".to_string(),
            failed_run_id: Some(publisher.clone()),
            observed,
        }
        .to_json();
        let recovery = self
            .jobs
            .insert_job_run(
                "blocked_task_recovery_pipeline",
                1,
                Utc::now(),
                Some(episode.clone()),
                None,
            )
            .unwrap()
            .run_id;
        (task, publisher, recovery, episode)
    }

    /// Apply a backstop decision through the registered
    /// `apply_blocked_task_recovery` deterministic action.
    fn apply_backstop(&self, recovery: &str, episode: &Value, result: Value) -> Value {
        use orbit_tools::{ReservationOwnerContext, ToolContext};
        RuntimeHost::run_deterministic(
            &self.runtime,
            "apply_blocked_task_recovery",
            &json!({}),
            &json!({
                "prepared": { "episode": episode, "recovery": { "base_ref": "main" } },
                "result": result,
            }),
            ToolContext {
                reservation_owner: Some(ReservationOwnerContext {
                    owner_run_id: recovery.to_string(),
                    owner_metadata: None,
                }),
                ..ToolContext::default()
            },
        )
        .unwrap()
    }

    fn history_events(&self, task: &str) -> Vec<String> {
        self.runtime
            .run_tool("orbit.task.show", json!({ "id": task }))
            .unwrap()["history"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["event"].as_str().map(str::to_string))
            .collect()
    }

    fn comments_text(&self, task: &str) -> String {
        self.runtime
            .run_tool("orbit.task.show", json!({ "id": task }))
            .unwrap()["comments"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|comment| comment["message"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

struct OutcomeWriteFault(rusqlite::Connection);

impl Drop for OutcomeWriteFault {
    fn drop(&mut self) {
        self.0
            .execute_batch(
                "DROP TRIGGER IF EXISTS fail_recovery_outcome; \
                 DROP TRIGGER IF EXISTS fail_recovery_outcome_insert;",
            )
            .unwrap();
    }
}

fn archive() -> FinalRecoveryDecision {
    FinalRecoveryDecision::Archive {
        reason: "superseded by the landed rewrite".to_string(),
    }
}

fn an_empty_crew_pool_skips_final_recovery_and_records_nothing() {
    let fixture = fixture("[]");
    let task = fixture.task();
    let run = fixture.running_run(&task);

    assert!(matches!(
        fixture.admit(&run, &task),
        FinalRecoveryAdmission::Skipped { .. }
    ));
    assert_eq!(fixture.state(&run).final_recovery, None);
}

fn final_recovery_is_admitted_once_and_a_resume_drops_the_stale_checkpoints() {
    for preserve_history in [false, true] {
        let fixture = fixture("[\"sol\"]");
        let task = fixture.task();
        let run = fixture.running_run(&task);
        let started = fixture.status(&task);

        assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);
        let checkpoint = fixture
            .state(&run)
            .final_recovery
            .expect("admission recorded");
        assert_eq!(checkpoint.task_id, task);
        assert_eq!(checkpoint.failed_step_id, "implement");
        assert_eq!(checkpoint.base_ref.as_deref(), Some("main"));
        assert!(
            checkpoint.observed.is_some(),
            "a local task's revision is observed"
        );
        assert!(
            matches!(
                fixture.admit(&run, &task),
                FinalRecoveryAdmission::Skipped { .. }
            ),
            "a second admission for the same run is refused"
        );

        if preserve_history {
            // Host history remains provenance while validation and compound
            // outputs from the replayed phase must be regenerated.
            let mut state = fixture.state(&run);
            state.record_step(
                3,
                JobRunState::Success,
                Some(json!({"validation": "old"})),
                None,
            );
            state.record_compound_outputs(
                1,
                std::collections::BTreeMap::from([("nested".into(), json!({"old": true}))]),
            );
            fixture.jobs.write_run_state(&run, &state).unwrap();
        }
        let resume = FinalRecoveryDecision::Resume {
            step_id: "implement".to_string(),
            rationale: "finished the implementation".to_string(),
        };
        let context = orbit_types::workflow::FinalRecoveryResume {
            step_id: "implement".to_string(),
            failed_step_id: "implement".to_string(),
            rationale: "finished the implementation".to_string(),
            refusal: "missing probe.command".to_string(),
            refused_output: json!({"deferred_sandbox_validation": {"probe": {}}}),
        };
        assert_eq!(
            RuntimeHost::apply_final_recovery(
                &fixture.runtime,
                &run,
                &FinalRecoveryApplication {
                    fault: Default::default(),
                    task_id: task.clone(),
                    failed_step_id: "implement".to_string(),
                    decision: resume.clone(),
                    resume_step_index: Some(1),
                    preserved_steps: if preserve_history { vec![2] } else { vec![] },
                    repair_commit: None,
                    resume_context: Some(context.clone()),
                    workspace_path: fixture.repo.clone(),
                    completion_done: false,
                    infrastructure: None,
                }
            )
            .unwrap(),
            FinalRecoveryApplied::Resume
        );
        let state = fixture.state(&run);
        assert_eq!(
            state.step_states.keys().copied().collect::<Vec<_>>(),
            if preserve_history {
                vec![0, 2]
            } else {
                vec![0]
            },
            "only host-selected history survives the resume point"
        );
        assert_eq!(
            state.step_outputs.keys().copied().collect::<Vec<_>>(),
            if preserve_history {
                vec![0, 2]
            } else {
                vec![0]
            }
        );
        assert!(
            state.compound_outputs.is_empty(),
            "nested and validation evidence must be regenerated"
        );
        let recorded = state.final_recovery.expect("decision recorded");
        assert_eq!(recorded.decision, Some(resume));
        assert_eq!(recorded.resume_context, Some(context));
        assert_eq!(fixture.status(&task), started, "resume writes no task");

        // A run resumed from this state inherits the record, so it is refused.
        assert!(matches!(
            fixture.admit(&run, &task),
            FinalRecoveryAdmission::Skipped { .. }
        ));
    }
}

fn a_resume_after_another_recorded_decision_is_refused_and_keeps_checkpoints() {
    let fixture = fixture("[\"sol\"]");
    let task = fixture.task();
    let run = fixture.running_run(&task);
    assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);
    fixture.apply(&run, &task, archive(), None);
    assert_eq!(fixture.state(&run).step_states.len(), 3);

    let refused = fixture.apply_result(
        &run,
        &task,
        FinalRecoveryDecision::Resume {
            step_id: "implement".to_string(),
            rationale: "retry the failed step".to_string(),
        },
        Some(1),
    );
    let message = refused
        .expect_err("resume must not replace archive")
        .to_string();
    assert!(
        message.contains("already recorded")
            && message.contains("archive")
            && message.contains("resume"),
        "{message}"
    );
    let state = fixture.state(&run);
    assert!(
        matches!(
            state
                .final_recovery
                .and_then(|checkpoint| checkpoint.decision),
            Some(FinalRecoveryDecision::Archive { .. })
        ),
        "the recorded decision stays archive"
    );
    assert_eq!(
        state.step_states.keys().copied().collect::<Vec<_>>(),
        vec![0, 1, 2],
        "a refused resume does not drop step checkpoints"
    );
}

fn a_local_decision_is_applied_to_the_task_through_the_applier() {
    let fixture = fixture("[\"sol\"]");
    let task = fixture.task();
    let run = fixture.running_run(&task);
    assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);

    let applied = fixture.apply(
        &run,
        &task,
        FinalRecoveryDecision::Archive {
            reason: "superseded by the landed rewrite".to_string(),
        },
        None,
    );

    assert!(
        matches!(applied, FinalRecoveryApplied::Settled { .. }),
        "{applied:?}"
    );
    assert_eq!(fixture.status(&task), "archived");
    let recorded = fixture.state(&run).final_recovery.unwrap();
    assert!(
        matches!(
            recorded.outcome,
            Some(FinalRecoveryCheckpointOutcome::Settled { .. })
        ),
        "the outcome is recorded with the decision"
    );
}

fn an_escalation_blocks_the_task_and_continues_into_the_failure_path() {
    let fixture = fixture("[\"sol\"]");
    let task = fixture.task();
    let run = fixture.running_run(&task);
    assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);

    let applied = fixture.apply(
        &run,
        &task,
        FinalRecoveryDecision::Escalate {
            diagnosis: "the provider rejected the push".to_string(),
            human_action: "grant the token repository write access".to_string(),
            summary: None,
        },
        None,
    );

    assert!(
        matches!(applied, FinalRecoveryApplied::Escalated { .. }),
        "{applied:?}"
    );
    assert_eq!(fixture.status(&task), "blocked");
}

fn a_settled_task_stands_when_recording_its_outcome_fails() {
    // Decided by the on-call ORB-13886 review of ORB-13907: a task mutation
    // must not be followed by failure_activity because the run-state write
    // after it failed.
    let fixture = fixture("[\"sol\"]");
    let task = fixture.task();
    let run = fixture.running_run(&task);
    assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);

    let fault = fixture.fail_outcome_writes();
    let applied = fixture.apply(&run, &task, archive(), None);
    drop(fault);

    assert!(
        matches!(applied, FinalRecoveryApplied::Settled { .. }),
        "the task took the decision, so the run settles instead of escalating \
         into failure_activity: {applied:?}"
    );
    assert_eq!(fixture.status(&task), "archived");
    let checkpoint = fixture.state(&run).final_recovery.unwrap();
    assert_eq!(
        checkpoint.decision,
        Some(archive()),
        "the intent was recorded before the task write"
    );
    assert_eq!(
        checkpoint.outcome, None,
        "the outcome write was the one lost"
    );
}

fn a_run_resumed_after_a_crash_mid_settlement_converges_on_one_outcome() {
    let fixture = fixture("[\"sol\"]");

    // Crash after the task write: the resumed run's replay finds the decision
    // already applied, writes nothing to the task, and settles the same way.
    let task = fixture.task();
    let run = fixture.running_run(&task);
    assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);
    let fault = fixture.fail_outcome_writes();
    fixture.apply(&run, &task, archive(), None);
    drop(fault);
    let before = fixture
        .runtime
        .run_tool("orbit.task.show", json!({ "id": task }))
        .unwrap();

    let resumed = fixture.resumed_run(&run);
    let replayed = fixture.apply(&resumed, &task, archive(), None);
    assert!(
        matches!(replayed, FinalRecoveryApplied::Settled { .. }),
        "{replayed:?}"
    );
    let after = fixture
        .runtime
        .run_tool("orbit.task.show", json!({ "id": task }))
        .unwrap();
    for field in ["status", "comments", "history", "updated_at"] {
        assert_eq!(after[field], before[field], "{field} changed on replay");
    }
    assert_eq!(fixture.decision_comments(&task), 1);
    assert!(
        fixture
            .state(&resumed)
            .final_recovery
            .unwrap()
            .outcome
            .is_some_and(|outcome| {
                matches!(outcome, FinalRecoveryCheckpointOutcome::Settled { .. })
            }),
        "the replay records the outcome the crash lost"
    );

    // Crash before the task write: only the intent is durable, so the replay
    // applies it, once.
    let task = fixture.task();
    let run = fixture.running_run(&task);
    assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);
    let mut state = fixture.state(&run);
    state.final_recovery.as_mut().unwrap().decision = Some(archive());
    fixture.jobs.write_run_state(&run, &state).unwrap();

    let resumed = fixture.resumed_run(&run);
    let applied = fixture.apply(&resumed, &task, archive(), None);
    assert!(
        matches!(applied, FinalRecoveryApplied::Settled { .. }),
        "{applied:?}"
    );
    assert_eq!(fixture.status(&task), "archived");
    assert_eq!(fixture.decision_comments(&task), 1);
}

fn final_recovery_log_tail_reads_only_the_requested_runs_bounded_log() {
    let fixture = fixture("[]");
    let logs = &fixture.runtime.paths().logs_dir;
    std::fs::create_dir_all(logs).unwrap();
    std::fs::write(
        logs.join("jrun-own.worker.log"),
        format!("discarded-prefix{}own-end", "界".repeat(100_000)),
    )
    .unwrap();
    std::fs::write(logs.join("jrun-other.worker.log"), "foreign-log").unwrap();
    let tail = RuntimeHost::final_recovery_log_tail(&fixture.runtime, "jrun-own")
        .unwrap()
        .unwrap();
    assert!(
        tail.len() < 64 * 1024,
        "worker log stays within the recovery text bound"
    );
    assert!(tail.ends_with("own-end"));
    assert!(!tail.contains("discarded-prefix"));
    assert!(!tail.contains("foreign-log"));
    assert_eq!(
        RuntimeHost::final_recovery_log_tail(&fixture.runtime, "jrun-missing").unwrap(),
        None
    );
    assert!(
        RuntimeHost::final_recovery_log_tail(&fixture.runtime, "../jrun-other").is_err(),
        "run identity cannot escape the log directory"
    );
}

fn final_recovery_keeps_run_observers_and_every_declared_tool_write_denied() {
    use orbit_common::OrbitError;
    use orbit_common::security::child_env::{
        ACTIVITY_NAME_ENV, ACTIVITY_TOOL_POLICY_ENV, ACTIVITY_TOOLS_DENY_ENV,
    };
    use orbit_types::workflow::ActivityV2Spec;

    let fixture = fixture("[]");
    for activity in [
        "final_recovery",
        "step_failure_recovery",
        "pr_conflict_recovery",
    ] {
        let yaml = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("assets/activities/{activity}.yaml")),
        )
        .unwrap();
        let asset = orbit_engine::activity_job::load_activity_asset(&yaml).unwrap();
        let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
            panic!("agent activity")
        };
        let denied = spec.tool_disallow_list.unwrap();
        for observer in ["orbit.workflow.run.show", "orbit.workflow.run.list"] {
            assert!(
                orbit_types::workflow::tool_allowed(observer, &denied),
                "ORB-14267: {activity} must withhold {observer}; recovery receives run evidence without operator authority"
            );
        }
        let resolved = fixture
            .runtime
            .resolve_activity_tool_denials(&[], activity, &denied)
            .unwrap();
        assert!(
            resolved
                .effective_tools
                .iter()
                .any(|tool| tool == "orbit.task.show")
        );
        for tool in &denied {
            assert!(
                !resolved.effective_tools.contains(tool),
                "denied tool {tool} must never be delegated to the harness"
            );
        }

        // An agent envelope alone cannot observe runs, even before the activity
        // deny list is applied. Advertising or requiring a tool grants no capability.
        {
            let _env = orbit_common::test_env::scoped([("ORBIT_AGENT_NAME", Some("codex"))]);
            for action in ["show", "list"] {
                let tool = format!("orbit.run:{action}");
                let error = fixture
                    .runtime
                    .execute_tool_command(
                        "orbit.run",
                        json!({"action": action, "id": "jrun-missing"}),
                        None,
                        None,
                    )
                    .unwrap_err();
                assert!(
                    matches!(error, OrbitError::CapabilityDenied(_)),
                    "{tool} must retain capability_denied for an agent envelope: {error}"
                );
            }
        }

        let deny_env = denied.join(",");
        let _env = orbit_common::test_env::scoped([
            ("ORBIT_AGENT_NAME", Some("codex")),
            ("ORBIT_TASK_ACTOR_KIND", Some("agent")),
            (ACTIVITY_NAME_ENV, Some(activity)),
            (ACTIVITY_TOOL_POLICY_ENV, Some("deny")),
            (ACTIVITY_TOOLS_DENY_ENV, Some(deny_env.as_str())),
        ]);
        for tool in &denied {
            // A denied family action is called through its family.
            let (name, input) = match tool.split_once(':') {
                Some((family, action)) => (family, json!({"action": action})),
                None => (tool.as_str(), json!({})),
            };
            let error = fixture
                .runtime
                .execute_tool_command(name, input, None, None)
                .unwrap_err();
            assert!(
                matches!(
                    error,
                    OrbitError::CapabilityDenied(_) | OrbitError::PolicyDenied(_)
                ),
                "{tool} must be rejected before domain execution: {error}"
            );
        }
    }
}

fn agent_loop_activities_withhold_run_show_and_keep_its_operator_gate() {
    use orbit_common::OrbitError;
    use orbit_types::workflow::ActivityV2Spec;

    let fixture = fixture("[]");
    for activity in [
        "agent_implement",
        "agent_review_repair",
        "task_pilot",
        "resident_cycle",
        "image_input_probe",
        "review_reconciliation_review",
        "triage_escalation",
    ] {
        let yaml = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("assets/activities/{activity}.yaml")),
        )
        .unwrap();
        let asset = orbit_engine::activity_job::load_activity_asset(&yaml).unwrap();
        let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
            panic!("agent activity")
        };
        let denied = spec.tool_disallow_list.unwrap();
        assert!(
            orbit_types::workflow::tool_allowed("orbit.workflow.run.show", &denied),
            "{activity} must withhold the operator-only run observer from its agent"
        );
        let resolved = fixture
            .runtime
            .resolve_activity_tool_denials(&[], activity, &denied)
            .unwrap();
        assert!(
            !orbit_types::workflow::tool_allowed(
                "orbit.workflow.run.show",
                &resolved.effective_tools
            ),
            "{activity} must not advertise the operator-only run observer"
        );
    }

    let _env = orbit_common::test_env::scoped([("ORBIT_AGENT_NAME", Some("codex"))]);
    let error = fixture
        .runtime
        .execute_tool_command(
            "orbit.run",
            json!({"action": "show", "id": "jrun-missing"}),
            None,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(error, OrbitError::CapabilityDenied(_)),
        "run.show remains operator-only for agent sessions: {error}"
    );
}

/// [ORB-15162] A task that requires `orbit.task.add` still gets a final-recovery
/// invocation. The tool stays out of the callable set, and the same requirement
/// under a covering deny list still fails the implementing activity.
fn final_recovery_drops_a_required_tool_its_disallow_list_covers() {
    let fixture = fixture("[]");
    let task_id = fixture.task_requiring(&["orbit.task.add", "orbit.task.show"]);
    let tasks = std::slice::from_ref(&task_id);

    let denied = shipped_disallow_list("final_recovery");
    assert!(
        denied.iter().any(|tool| tool == "orbit.task.add"),
        "ORB-15156: final_recovery disallows orbit.task.add"
    );
    let resolved = fixture
        .runtime
        .resolve_activity_tool_denials(tasks, "final_recovery", &denied)
        .expect("final recovery starts when the implementer requires a disallowed tool");
    assert!(
        !resolved
            .effective_tools
            .iter()
            .any(|tool| tool == "orbit.task.add"),
        "a requirement never overrides the disallow list"
    );
    assert!(
        !resolved
            .requested_tools
            .iter()
            .any(|tool| tool == "orbit.task.add"),
        "the covered requirement is dropped from requested_tools"
    );
    assert!(
        resolved
            .requested_tools
            .iter()
            .any(|tool| tool == "orbit.task.show"),
        "a requirement the disallow list does not cover stays requested"
    );
    assert!(
        resolved.omitted_requirement_notes.iter().any(|note| {
            note.contains(&task_id)
                && note.contains("orbit.task.add")
                && note.contains("final_recovery")
        }),
        "the drop is recorded: {:?}",
        resolved.omitted_requirement_notes
    );

    let covering = vec!["orbit.task.add".to_string()];
    let step = fixture
        .runtime
        .resolve_activity_tool_denials(tasks, "step_failure_recovery", &covering)
        .expect("step recovery drops a covered requirement instead of refusing to start");
    assert!(
        !step
            .effective_tools
            .iter()
            .any(|tool| tool == "orbit.task.add")
    );
    assert!(
        !step
            .requested_tools
            .iter()
            .any(|tool| tool == "orbit.task.add")
    );
    assert!(
        step.omitted_requirement_notes
            .iter()
            .any(|note| note.contains("step_failure_recovery"))
    );

    assert_admission_refused(&fixture, tasks, "agent_implement", &covering, &task_id);
    assert_admission_refused(&fixture, tasks, "custom_implementer", &covering, &task_id);
}

fn assert_admission_refused(
    fixture: &Fixture,
    tasks: &[String],
    activity: &str,
    disallow_list: &[String],
    task_id: &str,
) {
    let refused = fixture
        .runtime
        .resolve_activity_tool_denials(tasks, activity, disallow_list)
        .expect_err(
            "an implementing activity stays fail-closed when the list covers a requirement",
        );
    match refused {
        DispatchError::RequiredToolAdmission {
            task_id: refused_task,
            tool_name,
            reason,
        } => {
            assert_eq!(refused_task, task_id);
            assert_eq!(tool_name, "orbit.task.add");
            assert!(
                reason.contains("disallow list") && reason.contains(activity),
                "{activity} denial names the tool and the activity: {reason}"
            );
        }
        other => panic!("{activity}: expected RequiredToolAdmission, got {other}"),
    }
}

/// ORB-25253 (F2026-11-322): a stopped publisher plus a distinct blocked-task
/// recovery run returning `requeue` must not requeue: the recovery run cannot
/// preserve the publisher, so the task stays blocked with no preservation or
/// requeued evidence and the operator route named. A `rerun_ci` decision is
/// refused the same way, before any forge rerun, while an unpublished
/// backstop requeue still requeues.
fn backstop_requeue_of_an_unpreservable_published_episode_stays_blocked() {
    let fixture = fixture("[\"sol\"]");
    let fault = [
        ("fault", json!("implementer")),
        (
            "fault_evidence",
            json!("fixture: the publisher failed after opening its pull request"),
        ),
    ];

    // Published episode, `requeue` decision.
    let (task, publisher, recovery, episode) = fixture.blocked_backstop_episode(true);
    let mut fields: Vec<(&str, Value)> = vec![
        ("decision", json!("requeue")),
        ("reason", json!("the failure is fixed")),
    ];
    fields.extend(fault.iter().map(|(name, value)| (*name, value.clone())));
    let result = backstop_result(&fields);
    let applied = fixture.apply_backstop(&recovery, &episode, result);
    assert_eq!(applied["outcome"], "escalated", "{applied}");
    assert_eq!(fixture.status(&task), "blocked");
    let events = fixture.history_events(&task);
    assert!(
        !events
            .iter()
            .any(|event| event == "published_owner_requeued"),
        "no unauthenticated publisher preservation is recorded: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event == "final_recovery_requeued"),
        "no requeued source evidence is recorded: {events:?}"
    );
    let comments = fixture.comments_text(&task);
    assert!(
        comments.contains("backstop requeue refused")
            && comments.contains(&publisher)
            && comments.contains("orbit task requeue")
            && comments.contains("source_repair"),
        "the supported operator route is named: {comments}"
    );

    // Published episode, `rerun_ci` decision: refused before a forge rerun.
    let (task, publisher, recovery, episode) = fixture.blocked_backstop_episode(true);
    let mut fields: Vec<(&str, Value)> = vec![
        ("decision", json!("rerun_ci")),
        (
            "head_sha",
            json!("0123456789abcdef0123456789abcdef01234567"),
        ),
        ("reason", json!("rerun the failed checks")),
    ];
    fields.extend(fault.iter().map(|(name, value)| (*name, value.clone())));
    let applied = fixture.apply_backstop(&recovery, &episode, backstop_result(&fields));
    assert_eq!(applied["outcome"], "escalated", "{applied}");
    assert_eq!(fixture.status(&task), "blocked");
    let comments = fixture.comments_text(&task);
    assert!(
        comments.contains("backstop rerun_ci refused") && comments.contains(&publisher),
        "rerun_ci is refused at the same gate: {comments}"
    );
    assert!(
        !comments.contains("requested reruns for failed checks"),
        "no needless forge rerun runs first: {comments}"
    );

    // Unpublished episode: the ordinary backstop requeue still requeues.
    let (task, _publisher, recovery, episode) = fixture.blocked_backstop_episode(false);
    let mut fields: Vec<(&str, Value)> = vec![
        ("decision", json!("requeue")),
        ("reason", json!("the failure is fixed")),
    ];
    fields.extend(fault.iter().map(|(name, value)| (*name, value.clone())));
    let applied = fixture.apply_backstop(&recovery, &episode, backstop_result(&fields));
    assert_eq!(applied["outcome"], "requeued", "{applied}");
    assert_eq!(fixture.status(&task), "backlog");
}

/// The agent-step envelope `decision_result` extracts a decision from.
fn backstop_result(fields: &[(&str, Value)]) -> Value {
    let mut result = json!({
        "response_envelope_valid": true,
        "response_result_fields": fields.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
    });
    for (name, value) in fields {
        result[*name] = value.clone();
    }
    result
}

fn shipped_disallow_list(activity: &str) -> Vec<String> {
    use orbit_types::workflow::ActivityV2Spec;

    let yaml = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("assets/activities/{activity}.yaml")),
    )
    .unwrap();
    let asset = orbit_engine::activity_job::load_activity_asset(&yaml).unwrap();
    let ActivityV2Spec::AgentLoop(spec) = asset.spec.spec else {
        panic!("{activity} is an agent activity")
    };
    spec.tool_disallow_list.expect("deny-mode activity")
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    agent_loop_activity_tool_surface,
    agent_loop_activities_withhold_run_show_and_keep_its_operator_gate
);

crate::scenario_table!(
    an_empty_crew_pool_skips_final_recovery_and_records_nothing_scenarios,
    an_empty_crew_pool_skips_final_recovery_and_records_nothing,
    final_recovery_is_admitted_once_and_a_resume_drops_the_stale_checkpoints,
    a_local_decision_is_applied_to_the_task_through_the_applier,
    an_escalation_blocks_the_task_and_continues_into_the_failure_path,
    a_settled_task_stands_when_recording_its_outcome_fails,
    a_run_resumed_after_a_crash_mid_settlement_converges_on_one_outcome,
    final_recovery_log_tail_reads_only_the_requested_runs_bounded_log,
    final_recovery_keeps_run_observers_and_every_declared_tool_write_denied,
    final_recovery_drops_a_required_tool_its_disallow_list_covers
);

crate::scenario_table!(
    a_resume_after_another_recorded_decision_is_refused_and_keeps_checkpoints_scenarios,
    a_resume_after_another_recorded_decision_is_refused_and_keeps_checkpoints,
);

crate::scenario_table!(
    backstop_requeue_of_an_unpreservable_published_episode_scenarios,
    backstop_requeue_of_an_unpreservable_published_episode_stays_blocked,
);
