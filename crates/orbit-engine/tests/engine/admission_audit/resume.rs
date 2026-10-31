#![allow(missing_docs)]

//! [ORB-10002] Checkpoint/resume behavior of the v2 DAG executor:
//! per-step checkpoints flow through `RuntimeHost::checkpoint_step`, and
//! `execute_job_with_resume` skips checkpointed steps while feeding their
//! recorded outputs into the pipeline for later steps.

use std::collections::BTreeMap;
use std::sync::Mutex as StdMutex;

use chrono::{Duration, Utc};
use orbit_types::workflow::{
    AUTO_TASK_CHECKPOINT_VERSION, AutoTaskReceipt, AutoTaskRunCheckpoint, AutoTaskStepRecord,
    JobRunState, PipelineState,
};

use super::job_support::*;
use orbit_engine::{
    AutoTaskAdmission, AutoTaskObservation, AutoTaskReadiness, AutoTaskStepRequest, DispatchError,
    JobOutcome, RuntimeHost, execute_job_with_resume,
};
use orbit_types::workflow::activity_job::{
    FanInSpec, FanOutBlock, JobV2, JobV2Step, JobV2StepBody, JoinMode, LoopBlock, ParallelBlock,
    TargetRef,
};
use serde_json::{Value, json};

/// One `checkpoint_step` call: step index, step id, output.
type Checkpoint = (u32, String, Value);

/// Host wrapper that records `checkpoint_step` calls and dispatch inputs while
/// delegating dispatch to a `ScriptedHost`.
struct CheckpointHost {
    inner: ScriptedHost,
    checkpoints: StdMutex<Vec<Checkpoint>>,
    inputs: StdMutex<Vec<(String, Value)>>,
}

impl CheckpointHost {
    fn new(inner: ScriptedHost) -> Self {
        Self {
            inner,
            checkpoints: StdMutex::new(Vec::new()),
            inputs: StdMutex::new(Vec::new()),
        }
    }

    fn checkpoints(&self) -> Vec<Checkpoint> {
        self.checkpoints.lock().expect("checkpoints").clone()
    }

    fn inputs_for(&self, action: &str) -> Vec<Value> {
        self.inputs
            .lock()
            .expect("inputs")
            .iter()
            .filter_map(|(recorded_action, input)| {
                (recorded_action == action).then_some(input.clone())
            })
            .collect()
    }
}

impl orbit_engine::test_support::NullRuntimeHost for CheckpointHost {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.inputs
            .lock()
            .expect("inputs")
            .push((action.to_string(), input.clone()));
        RuntimeHost::run_deterministic(&self.inner, action, config, input, tool_context)
    }

    fn checkpoint_step(
        &self,
        _run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
        _compound_outputs: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        self.checkpoints.lock().expect("checkpoints").push((
            step_index,
            step_id.to_string(),
            output.clone(),
        ));
        Ok(())
    }
}

fn resume_state_with_completed_steps(steps: &[(u32, &str, Value)]) -> PipelineState {
    let mut state = PipelineState::new(
        "jrun-source".to_string(),
        "qa_resume".to_string(),
        Value::Object(Default::default()),
    );
    let mut pipeline = serde_json::Map::new();
    for (index, step_id, output) in steps {
        state.record_step(*index, JobRunState::Success, Some(output.clone()), None);
        pipeline.insert((*step_id).to_string(), output.clone());
    }
    state.sync_pipeline(Value::Object(pipeline));
    state
}

#[test]
fn resume_reexecuted_pr_output_reaches_promotion_and_checkpoint() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::resume_reexecuted_pr_output_reaches_promotion_and_checkpoint"
    )) {
        return;
    }

    // ORB-10241 / ORB-10240 incident shape: push completed in the source
    // attempt, pr_open failed, then resume skips push, re-executes pr_open,
    // and renders its numeric-looking string output into pr_promote.
    let host = CheckpointHost::new(ScriptedHost::new([
        (
            "test_git_push",
            vec![Action::Ok(json!({"pushed": "again"}))],
        ),
        (
            "test_pr_open",
            vec![Action::Ok(json!({
                "pr_number": "618",
                "pr_url": "https://github.example/pull/618",
            }))],
        ),
        (
            "test_pr_promote",
            vec![Action::Ok(json!({"promoted": true}))],
        ),
    ]));
    let mut promote = target_step("promote_tasks", "test_pr_promote");
    promote.when = Some("{{ steps.pr_open.output.pr_number }} == 618".to_string());
    let JobV2StepBody::Target(target) = &mut promote.body else {
        panic!("target step");
    };
    target.default_input = Some(json!({
        "pr_number": "{{ steps.pr_open.output.pr_number }}",
        "pr_url": "{{ steps.pr_open.output.pr_url }}",
    }));
    let job = job_with_steps(vec![
        target_step("push", "test_git_push"),
        target_step("pr_open", "test_pr_open"),
        promote,
    ]);
    let mut resume = resume_state_with_completed_steps(&[(0, "push", json!({"pushed": true}))]);
    resume.record_step(
        1,
        JobRunState::Failed,
        Some(json!({"pr_number": "stale"})),
        None,
    );
    resume.sync_pipeline(json!({
        "push": {"pushed": true},
        "pr_open": {"pr_number": "stale"},
    }));
    let writer = std::sync::Arc::new(test_writer("run-resume-pr-open"));

    let outcome = execute_job_with_resume(
        &job,
        Value::Null,
        "run-resume-pr-open",
        writer,
        &host,
        Some(&resume),
    )
    .expect("resume succeeds through promotion");

    assert!(outcome.success);
    assert_eq!(host.inner.call_count("test_git_push"), 0);
    assert_eq!(host.inner.call_count("test_pr_open"), 1);
    assert_eq!(host.inner.call_count("test_pr_promote"), 1);
    assert_eq!(
        host.inputs_for("test_pr_promote"),
        vec![json!({
            "pr_number": "618",
            "pr_url": "https://github.example/pull/618",
            "run_id": "run-resume-pr-open",
            "step_id": "promote_tasks",
        })],
        "fresh pr_open output retains its string type for downstream input",
    );

    let checkpoints = host.checkpoints();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0].0, 1);
    assert_eq!(checkpoints[0].1, "pr_open");
    assert_eq!(checkpoints[0].2["pr_number"], json!("618"));
    assert_eq!(resume.step_states.get(&1), Some(&JobRunState::Failed));
}

/// How the waiting auto-task branch answers on the next parked replay.
#[derive(Clone, Copy)]
enum CompoundReplayPhase {
    /// Readiness is still false. The branch parks before admission.
    Waiting,
    /// Readiness holds. The branch reserves, admits, and parks on the binding.
    Admit,
    /// The bound task has an accepted receipt. The branch settles.
    Receipt,
}

/// Host that parks an auto-task step and keeps succeeded parallel-branch
/// checkpoints, so a later `execute_job_with_resume` can restore them.
struct CompoundReplayHost {
    inner: ScriptedHost,
    phase: StdMutex<CompoundReplayPhase>,
    branches: StdMutex<BTreeMap<String, BTreeMap<String, Value>>>,
    records: StdMutex<BTreeMap<String, AutoTaskStepRecord>>,
    observes: StdMutex<u32>,
    inputs: StdMutex<Vec<(String, Value)>>,
}

impl CompoundReplayHost {
    fn new(inner: ScriptedHost) -> Self {
        Self {
            inner,
            phase: StdMutex::new(CompoundReplayPhase::Waiting),
            branches: StdMutex::new(BTreeMap::new()),
            records: StdMutex::new(BTreeMap::new()),
            observes: StdMutex::new(0),
            inputs: StdMutex::new(Vec::new()),
        }
    }

    fn set_phase(&self, phase: CompoundReplayPhase) {
        *self.phase.lock().expect("phase") = phase;
    }

    fn observe_count(&self) -> u32 {
        *self.observes.lock().expect("observes")
    }

    fn branches(&self) -> BTreeMap<String, BTreeMap<String, Value>> {
        self.branches.lock().expect("branches").clone()
    }

    fn inputs_for(&self, action: &str) -> Vec<Value> {
        self.inputs
            .lock()
            .expect("inputs")
            .iter()
            .filter_map(|(recorded, input)| (recorded == action).then_some(input.clone()))
            .collect()
    }

    fn resume_state(&self, run_id: &str) -> PipelineState {
        let now = Utc::now();
        let mut state =
            PipelineState::new(run_id.to_string(), "parked-compound".to_string(), json!({}));
        state.auto_task = Some(AutoTaskRunCheckpoint {
            version: AUTO_TASK_CHECKPOINT_VERSION,
            frozen_job: String::new(),
            frozen_targets: BTreeMap::new(),
            started_at: now,
            source_revision: None,
            expires_at: now + Duration::hours(24),
            steps: self.records.lock().expect("records").clone(),
            branches: self.branches(),
            parked: None,
        });
        state
    }
}

impl orbit_engine::test_support::NullRuntimeHost for CompoundReplayHost {
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: orbit_tools::ToolContext,
    ) -> Result<Value, DispatchError> {
        self.inputs
            .lock()
            .expect("inputs")
            .push((action.to_string(), input.clone()));
        RuntimeHost::run_deterministic(&self.inner, action, config, input, tool_context)
    }

    fn observe_auto_task_step(
        &self,
        _run_id: &str,
        request: &AutoTaskStepRequest,
    ) -> Result<AutoTaskObservation, DispatchError> {
        *self.observes.lock().expect("observes") += 1;
        let record = self
            .records
            .lock()
            .expect("records")
            .get(&request.step_id)
            .cloned();
        let now = Utc::now();
        let phase = *self.phase.lock().expect("phase");
        let (readiness, receipt) = match phase {
            CompoundReplayPhase::Waiting => {
                (AutoTaskReadiness::Waiting("board.empty".to_string()), None)
            }
            CompoundReplayPhase::Admit => (AutoTaskReadiness::Ready, None),
            CompoundReplayPhase::Receipt => {
                let task_id = record
                    .as_ref()
                    .and_then(|record| record.task_id.clone())
                    .unwrap_or_else(|| "T-HOLD".to_string());
                (
                    AutoTaskReadiness::Ready,
                    Some(AutoTaskReceipt {
                        task_id,
                        task_status: "done".to_string(),
                        accepted: true,
                        settled_at: now,
                        observed_at: now,
                    }),
                )
            }
        };
        Ok(AutoTaskObservation {
            now,
            record,
            action_key: format!("run:{}", request.step_id),
            input_digest: "digest".to_string(),
            definition_hold: None,
            receipt,
            bound_status: None,
            deadline: Some(now + Duration::hours(24)),
            dynamic_expiry: false,
            expired: None,
            readiness,
            recheck_at: now + Duration::seconds(30),
        })
    }

    fn checkpoint_auto_task_step(
        &self,
        _run_id: &str,
        step_id: &str,
        record: &AutoTaskStepRecord,
    ) -> Result<(), DispatchError> {
        self.records
            .lock()
            .expect("records")
            .insert(step_id.to_string(), record.clone());
        Ok(())
    }

    fn admit_auto_task_step(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> Result<AutoTaskAdmission, DispatchError> {
        Ok(AutoTaskAdmission::Bound {
            task_id: "T-HOLD".to_string(),
            minted: true,
        })
    }

    fn checkpoint_parallel_branch(
        &self,
        _run_id: &str,
        step_id: &str,
        branch_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        self.branches
            .lock()
            .expect("branches")
            .entry(step_id.to_string())
            .or_default()
            .insert(branch_id.to_string(), output.clone());
        Ok(())
    }

    fn park_job_run(
        &self,
        _run_id: &str,
        _park: &orbit_types::workflow::ParkedWait,
    ) -> Result<(), DispatchError> {
        Ok(())
    }
}

fn compound_replay_job() -> JobV2 {
    let wait = JobV2Step {
        id: "wait".to_string(),
        stage: None,
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        wait_until: Some("board.empty".to_string()),
        expire_when: None,
        reuse_when: None,
        require_result: None,
        body: JobV2StepBody::TargetRef(TargetRef {
            target: "auto-task:hold".to_string(),
            default_input: None,
            timeout_seconds: 0,
            session: None,
        }),
    };
    let work = JobV2Step {
        id: "work".to_string(),
        stage: None,
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        wait_until: None,
        expire_when: None,
        reuse_when: None,
        require_result: None,
        body: JobV2StepBody::Loop {
            loop_: LoopBlock {
                items: None,
                max_iterations: 1,
                timeout_seconds: None,
                interval_seconds: None,
                break_when: None,
                steps: vec![target_step("tick", "counter")],
            },
        },
    };
    let spread = JobV2Step {
        id: "spread".to_string(),
        stage: None,
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        wait_until: None,
        expire_when: None,
        reuse_when: None,
        require_result: None,
        body: JobV2StepBody::FanOut {
            fan_out: FanOutBlock {
                items: "[\"only\"]".to_string(),
                max_workers: 1,
                worker: Box::new(target_step("mark", "label")),
            },
            fan_in: FanInSpec {
                join: JoinMode::All,
                collect: Some("gathered".to_string()),
                errors: Some("spread_errors".to_string()),
            },
        },
    };
    let gate = JobV2Step {
        id: "gate".to_string(),
        stage: None,
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        wait_until: None,
        expire_when: None,
        reuse_when: None,
        require_result: None,
        body: JobV2StepBody::Parallel {
            parallel: ParallelBlock {
                join: JoinMode::All,
                branches: vec![wait, work, spread],
            },
        },
    };
    let mut read_back = target_step("read_back", "read_back");
    if let JobV2StepBody::Target(target) = &mut read_back.body {
        target.default_input = Some(json!({
            "ticks": "{{ steps.tick.output.n }}",
            "iterations": "{{ steps.work.output.iterations }}",
            "gathered": "{{ steps.gathered.output }}",
            "errors": "{{ steps.spread_errors.output }}",
        }));
    }
    job_with_steps(vec![gate, read_back])
}

fn assert_compound_sibling_restored(
    outcome: &JobOutcome,
    executions: usize,
    host: &CompoundReplayHost,
) {
    assert!(
        outcome.parked.is_some(),
        "an all-join with a still-waiting branch stays parked"
    );
    assert!(!outcome.success);
    assert_eq!(
        host.inner.call_count("counter"),
        executions,
        "a succeeded one-iteration counter must stay at one execution across parked replay"
    );
    assert_eq!(
        host.inner.call_count("label"),
        executions,
        "a succeeded fan-out worker must not run again while a sibling is parked"
    );
    assert_eq!(outcome.pipeline["tick"]["n"], json!(1));
    assert_eq!(outcome.pipeline["work"]["iterations"], json!(1));
    assert_eq!(outcome.pipeline["work"]["last_output"]["n"], json!(1));
    assert_eq!(outcome.pipeline["gathered"], json!([{"mark": "once"}]));
    assert_eq!(outcome.pipeline["spread"], json!([{"mark": "once"}]));
    assert_eq!(outcome.pipeline["spread_errors"], json!([null]));
}

/// [ORB-20434] A parked all-join must not rerun a succeeded loop or fan-out
/// sibling. Nested outputs and the fan-in aliases stay available, and the
/// waiting branch is still observed until it settles.
#[test]
fn parked_replay_keeps_succeeded_compound_branch_at_one_execution() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::parked_replay_keeps_succeeded_compound_branch_at_one_execution"
    )) {
        return;
    }

    let host = CompoundReplayHost::new(ScriptedHost::new([
        (
            "counter",
            vec![Action::Ok(json!({"n": 1})), Action::Ok(json!({"n": 2}))],
        ),
        (
            "label",
            vec![
                Action::Ok(json!({"mark": "once"})),
                Action::Ok(json!({"mark": "twice"})),
            ],
        ),
        ("read_back", vec![Action::Ok(json!({"read": true}))]),
    ]));
    let job = compound_replay_job();
    let run_id = "run-parked-compound";
    let writer = std::sync::Arc::new(test_writer(run_id));

    let first = execute_job_with_resume(&job, Value::Null, run_id, writer.clone(), &host, None)
        .expect("first pass parks");
    assert_compound_sibling_restored(&first, 1, &host);
    assert_eq!(host.observe_count(), 1);
    let work_checkpoint = host.branches()["gate"]["work"].clone();
    assert_eq!(work_checkpoint["kind"], json!("compound_branch"));
    assert_eq!(work_checkpoint["exposed"]["tick"]["n"], json!(1));
    assert_eq!(
        work_checkpoint["output"]["iterations"],
        json!(1),
        "the loop checkpoint keeps the one iteration that ran"
    );
    let spread_checkpoint = host.branches()["gate"]["spread"].clone();
    assert_eq!(spread_checkpoint["kind"], json!("compound_branch"));
    assert_eq!(
        spread_checkpoint["exposed"]["gathered"],
        json!([{"mark": "once"}])
    );
    assert_eq!(spread_checkpoint["exposed"]["spread_errors"], json!([null]));
    assert!(
        !host.branches()["gate"].contains_key("wait"),
        "a waiting branch is not checkpointed as succeeded"
    );

    let second = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&host.resume_state(run_id)),
    )
    .expect("second pass parks again");
    assert_compound_sibling_restored(&second, 1, &host);
    assert_eq!(
        host.observe_count(),
        2,
        "the unfinished auto-task branch is observed again"
    );

    host.set_phase(CompoundReplayPhase::Admit);
    let third = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&host.resume_state(run_id)),
    )
    .expect("admission still parks on the bound task");
    assert_compound_sibling_restored(&third, 1, &host);
    assert_eq!(host.observe_count(), 3);

    host.set_phase(CompoundReplayPhase::Receipt);
    let done = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer,
        &host,
        Some(&host.resume_state(run_id)),
    )
    .expect("accepted receipt lets the all-join finish");
    assert!(done.success);
    assert!(done.parked.is_none());
    assert_eq!(host.inner.call_count("counter"), 1);
    assert_eq!(host.inner.call_count("label"), 1);
    assert_eq!(host.inner.call_count("read_back"), 1);
    assert_eq!(host.observe_count(), 4);
    let read_back = &host.inputs_for("read_back")[0];
    assert_eq!(read_back["ticks"], json!(1));
    assert_eq!(read_back["iterations"], json!(1));
    assert_eq!(read_back["gathered"], json!([{"mark": "once"}]));
    assert_eq!(read_back["errors"], json!([null]));
    assert_eq!(read_back["step_id"], json!("read_back"));
    assert_eq!(done.pipeline["tick"]["n"], json!(1));
    assert_eq!(done.pipeline["gathered"], json!([{"mark": "once"}]));
}

fn step_with_body(id: &str, body: JobV2StepBody) -> JobV2Step {
    JobV2Step {
        id: id.to_string(),
        stage: None,
        when: None,
        retry: None,
        recovery_activity: None,
        resolved_recovery_activity: None,
        wait_until: None,
        expire_when: None,
        reuse_when: None,
        require_result: None,
        body,
    }
}

fn fan_out_branch(
    id: &str,
    items: &str,
    worker_id: &str,
    action: &str,
    collect: &str,
) -> JobV2Step {
    step_with_body(
        id,
        JobV2StepBody::FanOut {
            fan_out: FanOutBlock {
                items: items.to_string(),
                max_workers: 1,
                worker: Box::new(target_step(worker_id, action)),
            },
            fan_in: FanInSpec {
                join: JoinMode::All,
                collect: Some(collect.to_string()),
                errors: None,
            },
        },
    )
}

/// Seed writes `shared=[]`. The parked all-join then runs an empty fan-out
/// whose private worker id is that alias, beside the fan-out that actually
/// publishes `shared`.
fn alias_collision_replay_job() -> JobV2 {
    let seed = fan_out_branch("seed", "[]", "seed_worker", "seed_mark", "shared");
    // Worker id collides with the sibling alias. This branch never writes
    // parent `shared`; its collection is `b_results`.
    let empty = fan_out_branch("b", "[]", "shared", "b_mark", "b_results");
    let producer = fan_out_branch("a", "[\"one\"]", "a_worker", "a_mark", "shared");
    // Alias equals this fan-out's own worker id, which remains a parent write.
    let own_alias = fan_out_branch("c", "[\"one\"]", "c_kept", "c_mark", "c_kept");
    let mut hold = step_with_body(
        "hold",
        JobV2StepBody::TargetRef(TargetRef {
            target: "auto-task:hold".to_string(),
            default_input: None,
            timeout_seconds: 0,
            session: None,
        }),
    );
    hold.wait_until = Some("board.empty".to_string());
    let gate = step_with_body(
        "gate",
        JobV2StepBody::Parallel {
            parallel: ParallelBlock {
                join: JoinMode::All,
                branches: vec![empty, producer, own_alias, hold],
            },
        },
    );
    let mut read_back = target_step("read_back", "read_back");
    if let JobV2StepBody::Target(target) = &mut read_back.body {
        target.default_input = Some(json!({
            "shared": "{{ steps.shared.output }}",
            "b_results": "{{ steps.b_results.output }}",
            "kept": "{{ steps.c_kept.output }}",
        }));
    }
    job_with_steps(vec![seed, gate, read_back])
}

fn resume_alias_collision(host: &CompoundReplayHost, run_id: &str) -> PipelineState {
    let mut state = host.resume_state(run_id);
    // The seed fan-out completed before the park. Replaying it would publish
    // `shared=[]` again; resuming from its checkpoint leaves that inherited
    // empty collection for the parked block to correct.
    state.record_step(0, JobRunState::Success, Some(json!([])), None);
    state
}

fn assert_alias_collision_preserved(
    outcome: &JobOutcome,
    executions: usize,
    host: &CompoundReplayHost,
) {
    assert!(
        outcome.parked.is_some(),
        "an all-join with a still-waiting branch stays parked"
    );
    assert!(!outcome.success);
    assert_eq!(
        host.inner.call_count("a_mark"),
        executions,
        "the fan-out that publishes shared must stay at one execution across parked replay"
    );
    assert_eq!(
        host.inner.call_count("c_mark"),
        executions,
        "a fan-out whose alias equals its own worker id must stay at one execution"
    );
    assert_eq!(
        host.inner.call_count("b_mark"),
        0,
        "the empty fan-out never runs its private worker"
    );
    assert_eq!(host.inner.call_count("seed_mark"), 0);
    assert_eq!(
        outcome.pipeline["shared"],
        json!([{"tag": "new"}]),
        "restoring the empty fan-out must not replace the producer's collection"
    );
    assert_eq!(outcome.pipeline["b_results"], json!([]));
    assert_eq!(outcome.pipeline["c_kept"], json!([{"tag": "kept"}]));
}

/// [ORB-20452] A fan-in alias may equal another branch's fan-out worker id.
/// The empty fan-out's checkpoint must not capture the inherited value under
/// that private id, and replay must keep the real producer's collection no
/// matter which succeeded branch is restored first. Succeeded compound work
/// stays at one execution. An alias equal to the publishing fan-out's own
/// worker id is still checkpointed.
#[test]
fn parked_alias_equal_to_sibling_worker_id_keeps_the_producer_collection() {
    if !super::run_isolated(concat!(
        module_path!(),
        "::parked_alias_equal_to_sibling_worker_id_keeps_the_producer_collection"
    )) {
        return;
    }

    let host = CompoundReplayHost::new(ScriptedHost::new([
        (
            "a_mark",
            vec![
                Action::Ok(json!({"tag": "new"})),
                Action::Ok(json!({"tag": "reran"})),
            ],
        ),
        (
            "c_mark",
            vec![
                Action::Ok(json!({"tag": "kept"})),
                Action::Ok(json!({"tag": "reran"})),
            ],
        ),
        ("read_back", vec![Action::Ok(json!({"read": true}))]),
    ]));
    let job = alias_collision_replay_job();
    assert_eq!(job.steps[0].id, "seed");
    let run_id = "run-alias-worker-checkpoint";
    let writer = std::sync::Arc::new(test_writer(run_id));

    let first = execute_job_with_resume(&job, Value::Null, run_id, writer.clone(), &host, None)
        .expect("first pass parks");
    assert_alias_collision_preserved(&first, 1, &host);
    assert_eq!(host.observe_count(), 1);
    let branches = host.branches();
    let empty_checkpoint = &branches["gate"]["b"];
    assert_eq!(empty_checkpoint["kind"], json!("compound_branch"));
    assert_eq!(
        empty_checkpoint["exposed"],
        json!({"b_results": []}),
        "an empty fan-out must not checkpoint the inherited value under its private worker id"
    );
    let producer_checkpoint = &branches["gate"]["a"];
    assert_eq!(
        producer_checkpoint["exposed"],
        json!({"shared": [{"tag": "new"}]}),
        "only the fan-out that writes the alias checkpoints that collection"
    );
    let own_alias_checkpoint = &branches["gate"]["c"];
    assert_eq!(
        own_alias_checkpoint["exposed"],
        json!({"c_kept": [{"tag": "kept"}]}),
        "a fan-in alias equal to the same fan-out's worker id is still a parent entry"
    );
    assert!(
        !branches["gate"].contains_key("hold"),
        "a waiting branch is not checkpointed as succeeded"
    );
    // `b` does not record `shared`, so restoring `b` before or after `a`
    // cannot put the inherited empty collection back.

    let second = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&resume_alias_collision(&host, run_id)),
    )
    .expect("second pass parks again");
    assert_alias_collision_preserved(&second, 1, &host);
    assert_eq!(host.observe_count(), 2);

    host.set_phase(CompoundReplayPhase::Admit);
    let third = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer.clone(),
        &host,
        Some(&resume_alias_collision(&host, run_id)),
    )
    .expect("admission still parks on the bound task");
    assert_alias_collision_preserved(&third, 1, &host);
    assert_eq!(host.observe_count(), 3);

    host.set_phase(CompoundReplayPhase::Receipt);
    let done = execute_job_with_resume(
        &job,
        Value::Null,
        run_id,
        writer,
        &host,
        Some(&resume_alias_collision(&host, run_id)),
    )
    .expect("accepted receipt lets the all-join finish");
    assert!(done.success);
    assert!(done.parked.is_none());
    assert_eq!(host.inner.call_count("a_mark"), 1);
    assert_eq!(host.inner.call_count("c_mark"), 1);
    assert_eq!(host.inner.call_count("b_mark"), 0);
    assert_eq!(host.inner.call_count("seed_mark"), 0);
    assert_eq!(host.inner.call_count("read_back"), 1);
    assert_eq!(host.observe_count(), 4);
    assert_eq!(done.pipeline["shared"], json!([{"tag": "new"}]));
    assert_eq!(done.pipeline["b_results"], json!([]));
    assert_eq!(done.pipeline["c_kept"], json!([{"tag": "kept"}]));
    let read_back = &host.inputs_for("read_back")[0];
    assert_eq!(read_back["shared"], json!([{"tag": "new"}]));
    assert_eq!(read_back["b_results"], json!([]));
    assert_eq!(read_back["kept"], json!([{"tag": "kept"}]));
    assert_eq!(read_back["step_id"], json!("read_back"));
}
