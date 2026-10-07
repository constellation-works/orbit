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
    FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, RuntimeHost,
};
use orbit_store::contracts::JobRunStoreBackend;
use orbit_types::workflow::{FinalRecoveryDecision, JobRunState, PipelineState};
use serde_json::json;
use tempfile::TempDir;

mod repair_commit;
mod terminalization;

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    runtime: OrbitRuntime,
    repo: PathBuf,
    jobs: Arc<dyn JobRunStoreBackend>,
}

fn fixture(pool: &str) -> Fixture {
    let root = TempDir::new().unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(repo.join(".orbit")).unwrap();
    std::fs::write(
        repo.join(".orbit/config.toml"),
        format!(
            "[workflow]\ndefault_crew = \"sol\"\nfinal_recovery_crews = {pool}\n\n\
             [crews.sol]\nprovider = \"codex\"\nmodel = \"sol-model\"\n"
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
        // Named through the trait: `OrbitRuntime` also has the applier's own
        // `apply_final_recovery`, which the host method calls after recording.
        RuntimeHost::apply_final_recovery(
            &self.runtime,
            run,
            &FinalRecoveryApplication {
                task_id: task.to_string(),
                failed_step_id: "implement".to_string(),
                decision,
                resume_step_index,
                repair_commit: None,
                workspace_path: self.repo.clone(),
                completion_done: false,
            },
        )
        .unwrap()
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
                "CREATE TRIGGER fail_final_recovery_outcome \
                 BEFORE UPDATE ON job_run_states \
                 WHEN NEW.pipeline_state_json LIKE '%\"final_recovery\"%\"outcome\"%' \
                 BEGIN SELECT RAISE(ABORT, 'injected run-state write failure'); END; \
                 CREATE TRIGGER fail_final_recovery_outcome_insert \
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
}

struct OutcomeWriteFault(rusqlite::Connection);

impl Drop for OutcomeWriteFault {
    fn drop(&mut self) {
        self.0
            .execute_batch(
                "DROP TRIGGER IF EXISTS fail_final_recovery_outcome; \
                 DROP TRIGGER IF EXISTS fail_final_recovery_outcome_insert;",
            )
            .unwrap();
    }
}

fn archive() -> FinalRecoveryDecision {
    FinalRecoveryDecision::Archive {
        reason: "superseded by the landed rewrite".to_string(),
    }
}

#[test]
fn an_empty_crew_pool_skips_final_recovery_and_records_nothing() {
    if !super::dispatch_admission::isolated(
        "final_recovery::an_empty_crew_pool_skips_final_recovery_and_records_nothing",
    ) {
        return;
    }
    let fixture = fixture("[]");
    let task = fixture.task();
    let run = fixture.running_run(&task);

    assert!(matches!(
        fixture.admit(&run, &task),
        FinalRecoveryAdmission::Skipped { .. }
    ));
    assert_eq!(fixture.state(&run).final_recovery, None);
}

#[test]
fn final_recovery_is_admitted_once_and_a_resume_drops_the_stale_checkpoints() {
    if !super::dispatch_admission::isolated(
        "final_recovery::final_recovery_is_admitted_once_and_a_resume_drops_the_stale_checkpoints",
    ) {
        return;
    }
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

    let resume = FinalRecoveryDecision::Resume {
        step_id: "implement".to_string(),
        rationale: "finished the implementation".to_string(),
    };
    assert_eq!(
        fixture.apply(&run, &task, resume.clone(), Some(1)),
        FinalRecoveryApplied::Resume
    );
    let state = fixture.state(&run);
    assert_eq!(
        state.step_states.keys().copied().collect::<Vec<_>>(),
        [0],
        "steps from the resume point lose their checkpoints"
    );
    assert_eq!(state.step_outputs.keys().copied().collect::<Vec<_>>(), [0]);
    let recorded = state.final_recovery.expect("decision recorded");
    assert_eq!(recorded.decision, Some(resume));
    assert_eq!(fixture.status(&task), started, "resume writes no task");

    // A run resumed from this state inherits the record, so it is refused.
    assert!(matches!(
        fixture.admit(&run, &task),
        FinalRecoveryAdmission::Skipped { .. }
    ));
}

#[test]
fn a_local_decision_is_applied_to_the_task_through_the_applier() {
    if !super::dispatch_admission::isolated(
        "final_recovery::a_local_decision_is_applied_to_the_task_through_the_applier",
    ) {
        return;
    }
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
        recorded
            .outcome
            .is_some_and(|outcome| outcome.starts_with("settled")),
        "the outcome is recorded with the decision"
    );
}

#[test]
fn an_escalation_blocks_the_task_and_continues_into_the_failure_path() {
    if !super::dispatch_admission::isolated(
        "final_recovery::an_escalation_blocks_the_task_and_continues_into_the_failure_path",
    ) {
        return;
    }
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
        },
        None,
    );

    assert!(
        matches!(applied, FinalRecoveryApplied::Escalated { .. }),
        "{applied:?}"
    );
    assert_eq!(fixture.status(&task), "blocked");
}

#[test]
fn a_settled_task_stands_when_recording_its_outcome_fails() {
    if !super::dispatch_admission::isolated(
        "final_recovery::a_settled_task_stands_when_recording_its_outcome_fails",
    ) {
        return;
    }
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

#[test]
fn a_run_resumed_after_a_crash_mid_settlement_converges_on_one_outcome() {
    if !super::dispatch_admission::isolated(
        "final_recovery::a_run_resumed_after_a_crash_mid_settlement_converges_on_one_outcome",
    ) {
        return;
    }
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
            .is_some_and(|outcome| outcome.starts_with("settled")),
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

#[test]
fn final_recovery_log_tail_reads_only_the_requested_runs_bounded_log() {
    if !super::dispatch_admission::isolated(
        "final_recovery::final_recovery_log_tail_reads_only_the_requested_runs_bounded_log",
    ) {
        return;
    }
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

#[test]
fn final_recovery_keeps_run_observers_and_every_declared_tool_write_denied() {
    if !super::dispatch_admission::isolated(
        "final_recovery::final_recovery_keeps_run_observers_and_every_declared_tool_write_denied",
    ) {
        return;
    }
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
                denied.iter().any(|tool| tool == observer),
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
            for tool in ["orbit.workflow.run.show", "orbit.workflow.run.list"] {
                let error = fixture
                    .runtime
                    .execute_tool_command(tool, json!({"id": "jrun-missing"}), None, None)
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
            let error = fixture
                .runtime
                .execute_tool_command(tool, json!({}), None, None)
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
