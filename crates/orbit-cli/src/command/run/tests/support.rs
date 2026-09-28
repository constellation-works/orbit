use std::time::{Duration, Instant};

use orbit_core::OrbitRuntime;
use serde_json::json;

use super::super::support::*;

const SHIP_WORKFLOW: &str = "ship";

#[test]
fn async_ship_dispatch_returns_run_identity_without_waiting() {
    super::substitute_pipeline_worker();
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let jobs_dir = runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    std::fs::write(
        jobs_dir.join("task_auto_pipeline.yaml"),
        r#"schemaVersion: 2
kind: Job
metadata:
  name: task_auto_pipeline
spec:
  state: enabled
  kind: workflow
  steps:
    - id: marker
      spec:
        type: deterministic
        action: sleep
        config:
          seconds: 0
"#,
    )
    .expect("write task_auto_pipeline fixture");
    let started = Instant::now();
    let runs = dispatch_workflow(
        &runtime,
        SHIP_WORKFLOW,
        &json!({
            "mode": "pr",
            "base_branch": "main",
        }),
        false,
        false,
        1,
    )
    .expect("dispatch workflow");

    assert!(
        started.elapsed() < Duration::from_secs(1),
        "dispatch waited too long"
    );
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].workflow_alias, SHIP_WORKFLOW);
    assert_eq!(runs[0].job_id, "task_auto_pipeline");
    assert!(matches!(runs[0].state.as_str(), "submitted" | "queued"));
}

#[test]
fn async_dispatch_lines_point_to_history_and_show() {
    let run = WorkflowDispatchResult {
        workflow_alias: SHIP_WORKFLOW,
        job_id: "task_auto_pipeline".to_string(),
        run_id: "jrun-submitted".to_string(),
        state: "submitted".to_string(),
        attempt: 1,
        error_code: None,
        error_message: None,
    };

    assert_eq!(
        workflow_dispatch_result_lines(&run),
        vec![
            "Workflow: ship",
            "Job ID: task_auto_pipeline",
            "Run ID: jrun-submitted",
            "State: submitted",
            "Inspect: orbit run history -j task_auto_pipeline | orbit run show jrun-submitted",
        ]
    );
}

fn dispatch_result(state: &str, error_message: Option<&str>) -> WorkflowDispatchResult {
    WorkflowDispatchResult {
        workflow_alias: "task-pilot",
        job_id: "task_pilot_pipeline".to_string(),
        run_id: "jrun-pilot".to_string(),
        state: state.to_string(),
        attempt: 1,
        error_code: error_message.map(|_| "step_failed".to_string()),
        error_message: error_message.map(str::to_string),
    }
}

/// A waited workflow run that did not succeed must fail the command so shell
/// automation cannot proceed past a failed preflight, while the structured
/// result stays available for diagnosis.
#[test]
fn waited_dispatch_exits_nonzero_for_every_failing_terminal_state() {
    for state in ["failed", "timeout", "cancelled", "interrupted"] {
        let output = workflow_dispatch_payload(
            "task-pilot",
            &[dispatch_result(state, Some("pilot blew up"))],
        )
        .expect("a failing waited run still renders");
        let crate::command::CommandOutput::Payload(payload) = output else {
            panic!("failing waited run must return a payload, got {output:?}");
        };
        assert_eq!(payload.exit_code(), 1, "{state} wait must exit nonzero");
        let (doc, _) = payload.into_view();
        assert_eq!(doc["state"], state);
        assert_eq!(doc["run_id"], "jrun-pilot");
        assert_eq!(doc["error_code"], "step_failed");
        assert_eq!(doc["error_message"], "pilot blew up");
    }
}

#[test]
fn successful_wait_and_async_submissions_exit_zero() {
    for state in ["success", "submitted", "queued"] {
        let output = workflow_dispatch_payload("task-pilot", &[dispatch_result(state, None)])
            .expect("dispatch renders");
        let crate::command::CommandOutput::Payload(payload) = output else {
            panic!("dispatch must return a payload, got {output:?}");
        };
        assert_eq!(payload.exit_code(), 0, "{state} must exit zero");
        let (doc, _) = payload.into_view();
        assert_eq!(doc["state"], state);
    }
}

#[test]
fn any_failing_run_in_a_batch_fails_the_command() {
    let output = workflow_dispatch_payload(
        "task-pilot",
        &[
            dispatch_result("success", None),
            dispatch_result("failed", Some("pilot blew up")),
        ],
    )
    .expect("batch renders");
    let crate::command::CommandOutput::Payload(payload) = output else {
        panic!("batch must return a payload, got {output:?}");
    };
    assert_eq!(payload.exit_code(), 1);
    let (doc, _) = payload.into_view();
    assert_eq!(doc["runs"][1]["state"], "failed");
}
