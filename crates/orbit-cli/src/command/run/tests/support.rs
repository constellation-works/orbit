use std::time::{Duration, Instant};

use orbit_core::OrbitRuntime;
use serde_json::json;

use super::super::support::*;

const SHIP_WORKFLOW: &str = "ship";

#[test]
fn async_ship_dispatch_returns_run_identity_without_waiting() {
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
