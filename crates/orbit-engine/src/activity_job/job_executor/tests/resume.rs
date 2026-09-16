#![allow(missing_docs)]

//! [ORB-10002] Checkpoint/resume behavior of the v2 DAG executor:
//! per-step checkpoints flow through `RuntimeHost::checkpoint_step`, and
//! `execute_job_with_resume` skips checkpointed steps while feeding their
//! recorded outputs into the pipeline for later steps.

use std::sync::Mutex as StdMutex;

use orbit_types::workflow::{JobRunState, PipelineState};

use super::*;

/// Host wrapper that records `checkpoint_step` calls (and can inject
/// checkpoint failures) while delegating dispatch to a `ScriptedHost`.
struct CheckpointHost {
    inner: ScriptedHost,
    checkpoints: StdMutex<Vec<(u32, String, Value)>>,
    inputs: StdMutex<Vec<(String, Value)>>,
    fail_checkpoints: bool,
}

impl CheckpointHost {
    fn new(inner: ScriptedHost) -> Self {
        Self {
            inner,
            checkpoints: StdMutex::new(Vec::new()),
            inputs: StdMutex::new(Vec::new()),
            fail_checkpoints: false,
        }
    }

    fn failing(inner: ScriptedHost) -> Self {
        Self {
            fail_checkpoints: true,
            ..Self::new(inner)
        }
    }

    fn checkpoints(&self) -> Vec<(u32, String, Value)> {
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

impl RuntimeHost for CheckpointHost {
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
        self.inner
            .run_deterministic(action, config, input, tool_context)
    }

    fn resolve_cli_executor(
        &self,
        provider: &str,
    ) -> Result<crate::activity_job::dispatcher::ResolvedCliExecutor, DispatchError> {
        self.inner.resolve_cli_executor(provider)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<std::sync::Arc<dyn orbit_tools::FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> orbit_tools::ToolContext {
        self.inner
            .tool_context_for_activity(run_id, fs_profile, fs_audit, proc_allowed_programs)
    }

    fn checkpoint_step(
        &self,
        _run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        if self.fail_checkpoints {
            return Err(DispatchError::JobExecution(
                "injected checkpoint failure".to_string(),
            ));
        }
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
fn completed_top_level_steps_checkpoint_through_host() {
    let host = CheckpointHost::new(ScriptedHost::new([
        ("a0", vec![Action::Ok(json!({"v": 0}))]),
        ("a1", vec![Action::Ok(json!({"v": 1}))]),
    ]));
    let job = job_with_steps(vec![target_step("s0", "a0"), target_step("s1", "a1")]);
    let writer = std::sync::Arc::new(test_writer("run-ckpt"));

    let outcome =
        execute_job(&job, Value::Null, "run-ckpt", writer, &host).expect("execute_job ok");

    assert!(outcome.success);
    let checkpoints = host.checkpoints();
    assert_eq!(checkpoints.len(), 2, "one checkpoint per top-level step");
    assert_eq!(checkpoints[0].0, 0);
    assert_eq!(checkpoints[0].1, "s0");
    assert_eq!(checkpoints[0].2, json!({"v": 0}));
    assert_eq!(checkpoints[1].0, 1);
    assert_eq!(checkpoints[1].1, "s1");
    assert_eq!(
        checkpoints[1].2,
        json!({"v": 1}),
        "checkpoint payload is the completing step's output alone"
    );
}

#[test]
fn failing_step_is_not_checkpointed() {
    let host = CheckpointHost::new(ScriptedHost::new([
        ("a0", vec![Action::Ok(json!({"v": 0}))]),
        (
            "a1",
            vec![Action::Err(DispatchError::JobExecution("boom".into()))],
        ),
    ]));
    let job = job_with_steps(vec![target_step("s0", "a0"), target_step("s1", "a1")]);
    let writer = std::sync::Arc::new(test_writer("run-ckpt-fail"));

    let result = execute_job(&job, Value::Null, "run-ckpt-fail", writer, &host);

    assert!(result.is_err(), "failing step propagates");
    let checkpoints = host.checkpoints();
    assert_eq!(checkpoints.len(), 1, "only the completed step checkpointed");
    assert_eq!(checkpoints[0].1, "s0");
}

#[test]
fn checkpoint_write_failure_is_non_fatal() {
    let host = CheckpointHost::failing(ScriptedHost::new([(
        "a0",
        vec![Action::Ok(json!({"v": 0}))],
    )]));
    let job = job_with_steps(vec![target_step("s0", "a0")]);
    let writer = std::sync::Arc::new(test_writer("run-ckpt-nonfatal"));

    let outcome =
        execute_job(&job, Value::Null, "run-ckpt-nonfatal", writer, &host).expect("execute_job ok");

    assert!(
        outcome.success,
        "a checkpoint persistence failure must not fail the run"
    );
}

#[test]
fn resume_skips_checkpointed_steps_and_feeds_their_outputs() {
    // Simulates the post-crash second executor: step 0 completed before the
    // interruption, steps 1..2 still pending. Step 0's action must NOT run
    // again, and its recorded output must stay visible in the pipeline.
    let host = CheckpointHost::new(ScriptedHost::new([
        ("a0", vec![Action::Ok(json!({"v": "fresh-0"}))]),
        ("a1", vec![Action::Ok(json!({"v": 1}))]),
        ("a2", vec![Action::Ok(json!({"v": 2}))]),
    ]));
    let job = job_with_steps(vec![
        target_step("s0", "a0"),
        target_step("s1", "a1"),
        target_step("s2", "a2"),
    ]);
    let resume = resume_state_with_completed_steps(&[(0, "s0", json!({"v": "checkpointed-0"}))]);
    let writer = std::sync::Arc::new(test_writer("run-resume"));

    let outcome = execute_job_with_resume(
        &job,
        Value::Null,
        "run-resume",
        writer,
        &host,
        Some(&resume),
    )
    .expect("execute_job_with_resume ok");

    assert!(outcome.success);
    assert_eq!(host.inner.call_count("a0"), 0, "step 0 must not re-execute");
    assert_eq!(host.inner.call_count("a1"), 1);
    assert_eq!(host.inner.call_count("a2"), 1);
    let pipeline = outcome.pipeline.as_object().expect("pipeline obj");
    assert_eq!(
        pipeline.get("s0"),
        Some(&json!({"v": "checkpointed-0"})),
        "skipped step's checkpointed output is fed into the pipeline"
    );
    assert_eq!(pipeline.get("s1"), Some(&json!({"v": 1})));
    assert_eq!(pipeline.get("s2"), Some(&json!({"v": 2})));
    // Only the re-executed steps checkpoint; the skipped step's checkpoint
    // is already durable in the source run.
    let checkpoints = host.checkpoints();
    assert_eq!(checkpoints.len(), 2);
    assert_eq!(checkpoints[0].0, 1);
    assert_eq!(checkpoints[0].2, json!({"v": 1}));
    assert_eq!(checkpoints[1].0, 2);
}

#[test]
fn resume_reexecuted_pr_output_reaches_promotion_and_checkpoint() {
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

/// [ORB-10470] The `jrun-20260725-2246-3` sequence (F2026-07-121 /
/// F2026-07-122): `worktree` … `sync_base` succeeded, `push` failed. The
/// resume must start at `push`, must not re-dispatch the agent implementation,
/// and must keep handing the delivery tail the batch id the checkpointed
/// `worktree` output carries — the identity the durable task record is
/// reconciled to. Re-resuming the result is then a no-op for `push`.
#[test]
fn resume_starts_at_the_failed_push_and_reuses_checkpoints_idempotently() {
    let delivery_job = || {
        let mut push = target_step("push", "test_git_push");
        let JobV2StepBody::Target(target) = &mut push.body else {
            panic!("target step");
        };
        target.default_input = Some(json!({
            "job_run_id": "{{ steps.worktree.output.job_run_id }}",
            "workspace_path": "{{ steps.worktree.output.workspace_path }}",
        }));
        let mut pr_open = target_step("pr_open", "test_pr_open");
        let JobV2StepBody::Target(target) = &mut pr_open.body else {
            panic!("target step");
        };
        target.default_input = Some(json!({
            "job_run_id": "{{ steps.worktree.output.job_run_id }}",
        }));
        job_with_steps(vec![
            target_step("worktree", "test_worktree_setup"),
            target_step("implement_bundle", "agent_implement"),
            target_step("commit", "test_git_commit"),
            target_step("prepare_branch", "test_pr_prepare"),
            target_step("sync_base", "test_git_rebase"),
            push,
            pr_open,
        ])
    };
    let scripted = || {
        ScriptedHost::new([
            (
                "test_worktree_setup",
                vec![Action::Ok(json!({"replayed": true}))],
            ),
            (
                "agent_implement",
                vec![Action::Ok(json!({"replayed": true}))],
            ),
            (
                "test_git_commit",
                vec![Action::Ok(json!({"replayed": true}))],
            ),
            (
                "test_pr_prepare",
                vec![Action::Ok(json!({"replayed": true}))],
            ),
            (
                "test_git_rebase",
                vec![Action::Ok(json!({"replayed": true}))],
            ),
            ("test_git_push", vec![Action::Ok(json!({"pushed": true}))]),
            (
                "test_pr_open",
                vec![Action::Ok(json!({"pr_number": "711"}))],
            ),
        ])
    };

    let mut resume = resume_state_with_completed_steps(&[
        (
            0,
            "worktree",
            json!({"job_run_id": "jrun-source", "workspace_path": "/wt/jrun-source"}),
        ),
        (1, "implement_bundle", json!({"implemented": true})),
        (2, "commit", json!({"commit_sha": "7387d771"})),
        (3, "prepare_branch", json!({"phase": "prepare"})),
        (4, "sync_base", json!({"decision": "already_fresh"})),
    ]);
    resume.record_step(5, JobRunState::Failed, None, None);

    let host = CheckpointHost::new(scripted());
    let outcome = execute_job_with_resume(
        &delivery_job(),
        Value::Null,
        "jrun-resume-1",
        std::sync::Arc::new(test_writer("jrun-resume-1")),
        &host,
        Some(&resume),
    )
    .expect("resume reaches the delivery tail");

    assert!(outcome.success);
    for skipped in [
        "test_worktree_setup",
        "agent_implement",
        "test_git_commit",
        "test_pr_prepare",
        "test_git_rebase",
    ] {
        assert_eq!(
            host.inner.call_count(skipped),
            0,
            "resume must not re-execute the checkpointed step `{skipped}`",
        );
    }
    assert_eq!(host.inner.call_count("test_git_push"), 1);
    assert_eq!(host.inner.call_count("test_pr_open"), 1);
    assert_eq!(
        host.inputs_for("test_git_push"),
        vec![json!({
            "job_run_id": "jrun-source",
            "workspace_path": "/wt/jrun-source",
            "run_id": "jrun-resume-1",
            "step_id": "push",
        })],
        "the delivery tail keeps the checkpointed batch id as its ownership id",
    );

    // The resumed run's own checkpoints resume again without re-pushing —
    // what a worker restart (or a second resume) does.
    let mut second = resume.clone();
    for (index, step_id, output) in host.checkpoints() {
        second.record_step(index, JobRunState::Success, Some(output.clone()), None);
        second.record_pipeline_output(&step_id, output);
    }
    let replay_host = CheckpointHost::new(scripted());
    let replayed = execute_job_with_resume(
        &delivery_job(),
        Value::Null,
        "jrun-resume-2",
        std::sync::Arc::new(test_writer("jrun-resume-2")),
        &replay_host,
        Some(&second),
    )
    .expect("second resume is a no-op");

    assert!(replayed.success);
    assert_eq!(
        replay_host.inner.call_count("test_git_push"),
        0,
        "checkpoint reuse is idempotent: a completed push is never repeated",
    );
    assert_eq!(replay_host.inner.call_count("test_pr_open"), 0);
    assert!(replay_host.checkpoints().is_empty());
}

#[test]
fn resume_seed_uses_only_successful_checkpoint_outputs() {
    let job = job_with_steps(vec![
        target_step("success", "a0"),
        target_step("failed", "a1"),
        target_step("timed_out", "a2"),
    ]);
    let mut resume = PipelineState::new(
        "jrun-source".to_string(),
        "qa_resume".to_string(),
        Value::Object(Default::default()),
    );
    resume.record_step(0, JobRunState::Success, Some(json!({"v": 0})), None);
    resume.record_step(1, JobRunState::Failed, Some(json!({"v": 1})), None);
    resume.record_step(2, JobRunState::Timeout, Some(json!({"v": 2})), None);
    resume.sync_pipeline(json!({
        "success": {"v": 0},
        "failed": {"v": 1},
        "timed_out": {"v": 2},
    }));

    assert_eq!(
        seed_pipeline_from_resume(&job, Some(&resume)),
        HashMap::from([("success".to_string(), json!({"v": 0}))]),
    );
}

#[test]
fn resume_with_no_completed_steps_runs_everything() {
    let host = CheckpointHost::new(ScriptedHost::new([
        ("a0", vec![Action::Ok(json!({"v": 0}))]),
        ("a1", vec![Action::Ok(json!({"v": 1}))]),
    ]));
    let job = job_with_steps(vec![target_step("s0", "a0"), target_step("s1", "a1")]);
    let resume = PipelineState::new(
        "jrun-source".to_string(),
        "qa_resume".to_string(),
        Value::Object(Default::default()),
    );
    let writer = std::sync::Arc::new(test_writer("run-resume-empty"));

    let outcome = execute_job_with_resume(
        &job,
        Value::Null,
        "run-resume-empty",
        writer,
        &host,
        Some(&resume),
    )
    .expect("execute_job_with_resume ok");

    assert!(outcome.success);
    assert_eq!(host.inner.call_count("a0"), 1);
    assert_eq!(host.inner.call_count("a1"), 1);
}

#[test]
fn resume_ignores_non_success_step_states() {
    // A step recorded as `failed` in the source run must re-execute.
    let host = CheckpointHost::new(ScriptedHost::new([(
        "a0",
        vec![Action::Ok(json!({"v": 0}))],
    )]));
    let job = job_with_steps(vec![target_step("s0", "a0")]);
    let mut resume = PipelineState::new(
        "jrun-source".to_string(),
        "qa_resume".to_string(),
        Value::Object(Default::default()),
    );
    resume.record_step(0, JobRunState::Failed, Some(json!({"v": "stale"})), None);
    let writer = std::sync::Arc::new(test_writer("run-resume-failed-step"));

    let outcome = execute_job_with_resume(
        &job,
        Value::Null,
        "run-resume-failed-step",
        writer,
        &host,
        Some(&resume),
    )
    .expect("execute_job_with_resume ok");

    assert!(outcome.success);
    assert_eq!(
        host.inner.call_count("a0"),
        1,
        "failed step must re-execute on resume"
    );
}
