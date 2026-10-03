#![allow(missing_docs)]

//! [ORB-10002] Checkpoint/resume behavior of the v2 DAG executor:
//! per-step checkpoints flow through `RuntimeHost::checkpoint_step`, and
//! `execute_job_with_resume` skips checkpointed steps while feeding their
//! recorded outputs into the pipeline for later steps.

use std::collections::BTreeMap;
use std::sync::Mutex as StdMutex;

use orbit_types::workflow::{JobRunState, PipelineState};

use super::*;

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
