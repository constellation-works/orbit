use std::path::PathBuf;

use chrono::Utc;
use orbit_core::{JobRun, NotFoundKind, OrbitError, OrbitRuntime, V2AuditEventInsertParams};
use orbit_types::workflow::{
    ChildDispatch, ChildDispatchPhase, JobRunState, JobRunStep, JobTargetType, PipelineState,
};
use serde_json::{Value, json};

use crate::command::Execute;

use super::super::job::*;
use super::super::{RunRead, run_show_payload};

fn test_run(state: JobRunState) -> JobRun {
    let now = Utc::now();
    JobRun {
        executed_on: None,
        run_id: "jrun-test".to_string(),
        job_id: "task_gate_pipeline".to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: None,
        duration_ms: None,
        created_at: now,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    }
}

fn persist_failed_show_run(
    runtime: &OrbitRuntime,
    run_id: &str,
    step_id: &str,
    error: &str,
    children: &[&str],
) {
    let mut run = test_run(JobRunState::Failed);
    run.run_id = run_id.to_string();
    run.finished_at = run.started_at;
    run.steps.push(JobRunStep {
        step_index: 0,
        target_type: JobTargetType::Activity,
        target_id: step_id.to_string(),
        started_at: run.started_at,
        finished_at: run.finished_at,
        duration_ms: Some(1),
        exit_code: Some(1),
        agent_response_json: None,
        state: JobRunState::Failed,
        error_code: Some("step_failed".to_string()),
        error_message: Some(error.to_string()),
    });
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    for child_id in children {
        state.record_child_dispatch(
            ChildDispatch::submitted(
                (*child_id).to_string(),
                "task_gate_pipeline".to_string(),
                "invoke_and_wait".to_string(),
                true,
                false,
                Utc::now(),
            )
            .with_parent_step_id(Some(step_id.to_string())),
        );
        state.advance_child_dispatch(
            child_id,
            ChildDispatchPhase::Terminal,
            Some("failed".to_string()),
            Some(error.to_string()),
        );
    }
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let store = runtime.sqlite_store().expect("store");
    store
        .upsert_job_run_for_workspace(&workspace_id, &run, Some(&state))
        .expect("persist run");
    store
        .upsert_job_run_step_for_workspace(&workspace_id, run_id, &run.steps[0])
        .expect("persist step");
}

fn persist_failed_audit_leaf(runtime: &OrbitRuntime, run_id: &str, step_id: &str, error: &str) {
    // V2 can keep a synthetic job-level wrapper step alongside the actual
    // activity failure in its audit trail.
    persist_failed_show_run(runtime, run_id, "job", "synthetic wrapper error", &[]);
    let workspace_id = runtime.workspace_id().expect("workspace id");
    for (index, (event_id, mut body)) in [
        ("step-started", json!({"body_kind": "step_started", "step_id": step_id})),
        (
            "step-finished",
            json!({"body_kind": "step_finished", "step_id": step_id, "outcome": "error", "error_message": error}),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let ts = Utc::now() + chrono::Duration::milliseconds(index as i64);
        body["event_id"] = json!(event_id);
        body["ts"] = json!(ts.to_rfc3339());
        runtime
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: workspace_id.clone(),
                event_id: event_id.to_string(),
                source: "v2_envelope".to_string(),
                schema_version: 1,
                event_type: "test.event".to_string(),
                ts,
                run_id: run_id.to_string(),
                agent_identity: "codex".to_string(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: body.to_string(),
            })
            .expect("persist audit step event");
    }
}

#[test]
fn run_show_reports_all_failed_leaves_depth_first_with_complete_errors() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let long_error = format!(
        "{}base branch checkout must be clean before merge_batch_worktree_into_base",
        "worktree setup context; ".repeat(8),
    );
    persist_failed_show_run(
        &runtime,
        "jrun-top",
        "pipeline_success_guard",
        "top-level wrapper failure",
        &["jrun-gate", "jrun-other"],
    );
    persist_failed_show_run(
        &runtime,
        "jrun-gate",
        "pipeline_success_guard",
        "gate wrapper failure",
        &["jrun-local", "jrun-local-second"],
    );
    persist_failed_audit_leaf(&runtime, "jrun-local", "worktree_setup", &long_error);
    persist_failed_show_run(
        &runtime,
        "jrun-local-second",
        "validate",
        "second leaf failed",
        &[],
    );
    persist_failed_show_run(&runtime, "jrun-other", "publish", "third leaf failed", &[]);

    let output = run_show_payload(&runtime, Some("jrun-top"), None, RunRead::Observe)
        .expect("show top-level run");
    let crate::command::CommandOutput::Payload(payload) = output else {
        panic!("run show should produce a payload");
    };
    let (document, view) = payload.into_view();
    assert_eq!(document["root_cause"]["run_id"], "jrun-local");
    assert_eq!(document["root_cause"]["step"], "worktree_setup");
    assert_eq!(document["root_cause"]["message"], long_error);
    assert_eq!(
        document["additional_root_causes"],
        json!([
            {"run_id": "jrun-local-second", "step": "validate", "message": "second leaf failed"},
            {"run_id": "jrun-other", "step": "publish", "message": "third leaf failed"},
        ])
    );
    assert_eq!(
        document["run"]["error_message"],
        "top-level wrapper failure"
    );

    let crate::output::payload::View::Blocks(blocks) = view else {
        panic!("run show should include a human view");
    };
    let crate::output::payload::Block::Text(header) = &blocks[0] else {
        panic!("run show should start with a text header");
    };
    assert!(header.contains(&format!(
        "Root cause: run=jrun-local step=worktree_setup error={long_error}"
    )));
    let second = header
        .find("Additional root cause: run=jrun-local-second step=validate error=second leaf failed")
        .expect("second leaf line");
    let third = header
        .find("Additional root cause: run=jrun-other step=publish error=third leaf failed")
        .expect("third leaf line");
    assert!(
        second < third,
        "failed leaves should follow child dispatch order"
    );
    assert!(
        header.contains("Child jrun-gate job="),
        "wrapper lineage remains visible"
    );
}

fn write_replay_job(runtime: &OrbitRuntime, name: &str) -> PathBuf {
    let jobs_dir = runtime.data_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).expect("create jobs dir");
    let path = jobs_dir.join(format!("{name}.yaml"));
    std::fs::write(
        &path,
        format!(
            r#"schemaVersion: 2
kind: Job
metadata:
  name: {name}
spec:
  state: enabled
  kind: workflow
  steps:
    - id: nap
      spec:
        type: deterministic
        action: sleep
        config: {{}}
"#
        ),
    )
    .expect("write replay job");
    path
}

#[test]
fn job_run_json_includes_waiting_reasons_from_state() {
    let run = test_run(JobRunState::Running);
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.set_waiting_reasons(
        Some(vec!["ORB-1".to_string()]),
        Some(vec!["file:src/lib.rs".to_string()]),
    );

    let value = cli_job_run_to_json(&run, Some(&state));

    assert_eq!(value["waiting_on_deps"], json!(["ORB-1"]));
    assert_eq!(value["waiting_on_locks"], json!(["file:src/lib.rs"]));
    assert_eq!(value["pid"], Value::Null);
}

#[test]
fn job_run_json_omits_stale_waiting_reasons_for_terminal_run() {
    let run = test_run(JobRunState::Success);
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.set_waiting_reasons(
        Some(vec!["ORB-1".to_string()]),
        Some(vec!["file:src/lib.rs".to_string()]),
    );

    let value = cli_job_run_to_json(&run, Some(&state));

    assert_eq!(value["waiting_on_deps"], Value::Null);
    assert_eq!(value["waiting_on_locks"], Value::Null);
}

#[test]
fn job_replay_args_execute_creates_linked_run() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let job_path = write_replay_job(&runtime, "cli_replay_success");
    let source = runtime
        .run_job_v2_from_yaml(&job_path, json!({ "seconds": 0 }))
        .expect("source run");

    JobReplayArgs {
        run_id: source.run_id.clone(),
        json: true,
    }
    .execute(&runtime)
    .expect("replay run");

    let history = runtime
        .job_history("cli_replay_success")
        .expect("job history");
    assert!(history.iter().any(|run| {
        run.retry_source_run_id.as_deref() == Some(source.run_id.as_str())
            && run.state == orbit_types::workflow::JobRunState::Success
    }));
}

#[test]
fn job_replay_args_execute_unknown_run_returns_error() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let error = JobReplayArgs {
        run_id: "jrun-missing".to_string(),
        json: true,
    }
    .execute(&runtime)
    .expect_err("unknown source run should fail");

    assert!(matches!(
        error,
        OrbitError::NotFound {
            kind: NotFoundKind::JobRun,
            ..
        }
    ));
}

// --- [ORB-10801] submission-mode output and exit status ---------------------

fn invoke_result(queued: bool) -> orbit_core::PipelineInvokeResult {
    orbit_core::PipelineInvokeResult {
        run_id: "jrun-20260815-0001".to_string(),
        job_name: "task_pilot_pipeline".to_string(),
        submitted_at: "2026-08-15T00:00:00Z".to_string(),
        queued,
    }
}

fn wait_entry(status: &str, error: Option<&str>) -> orbit_core::PipelineWaitEntry {
    orbit_core::PipelineWaitEntry {
        run_id: "jrun-20260815-0001".to_string(),
        status: status.to_string(),
        finished_at: Some("2026-08-15T00:01:00Z".to_string()),
        duration_ms: Some(1000),
        pipeline: None,
        error: error.map(ToOwned::to_owned),
    }
}

/// The default submission tells an operator the run id, whether it started or
/// queued, and how to look at it — the three things they need once the command
/// stops blocking.
#[test]
fn submission_output_names_the_run_state_and_how_to_inspect_it() {
    for (queued, expected_state) in [(false, "submitted"), (true, "queued")] {
        let invoke = invoke_result(queued);
        let state = submission_state(&invoke);
        assert_eq!(state, expected_state);

        let lines = submission_lines(&invoke, state).join("\n");
        assert!(lines.contains("Run ID: jrun-20260815-0001"), "{lines}");
        assert!(
            lines.contains(&format!("State: {expected_state}")),
            "{lines}"
        );
        assert!(
            lines.contains("orbit run history -j task_pilot_pipeline"),
            "{lines}"
        );
        assert!(
            lines.contains("orbit run show jrun-20260815-0001"),
            "{lines}"
        );
    }
}

/// Submission mode reports on the submission, not the eventual job outcome, so
/// a successful submission always exits zero.
#[test]
fn submission_without_wait_succeeds() {
    render_submission(&invoke_result(false)).expect("submission renders and exits zero");
    render_submission(&invoke_result(true)).expect("queued submission exits zero too");
}

#[test]
fn submission_payload_is_a_json_document_with_a_human_view() {
    let output = render_submission(&invoke_result(false)).expect("submission payload");
    let crate::command::CommandOutput::Payload(payload) = output else {
        panic!("submission must not return Silent, got {output:?}");
    };
    let (doc, view) = payload.into_view();
    assert_eq!(doc["run_id"], "jrun-20260815-0001");
    assert_eq!(doc["job_id"], "task_pilot_pipeline");
    assert_eq!(doc["waited"], false);
    let crate::output::payload::View::Blocks(blocks) = view else {
        panic!("submission must keep a human view");
    };
    let crate::output::payload::Block::Text(text) = &blocks[0] else {
        panic!("submission human view is prose");
    };
    assert!(text.contains("Run ID: jrun-20260815-0001"), "{text}");
}

/// `--wait` is the only mode that reports the run's own outcome, and it maps
/// every non-success terminal state onto a nonzero exit.
#[test]
fn wait_exits_nonzero_for_every_failing_terminal_state() {
    for status in ["failed", "timeout", "cancelled", "interrupted"] {
        let invoke = invoke_result(false);
        let entry = wait_entry(status, Some("step_failed: implement blew up"));
        let output = render_wait(&invoke, &entry).expect("a failing wait still renders");
        let crate::command::CommandOutput::Payload(payload) = output else {
            panic!("failing wait must return a payload, got {output:?}");
        };
        assert_eq!(payload.exit_code(), 1, "failing wait must exit nonzero");
        let (doc, view) = payload.into_view();
        assert_eq!(doc["state"], status);
        assert_eq!(doc["run_id"], "jrun-20260815-0001");
        assert_eq!(doc["error"], "step_failed: implement blew up");
        let crate::output::payload::View::Blocks(blocks) = view else {
            panic!("failing wait must keep a human view");
        };
        let crate::output::payload::Block::Text(text) = &blocks[0] else {
            panic!("failing wait human view is prose");
        };
        assert!(text.contains(status), "{text}");
        assert!(text.contains("jrun-20260815-0001"), "{text}");
        assert!(text.contains("implement blew up"), "{text}");
    }
}

#[test]
fn wait_exits_zero_when_the_run_succeeded() {
    let output = render_wait(&invoke_result(false), &wait_entry("success", None))
        .expect("a successful run must exit zero");
    let crate::command::CommandOutput::Payload(payload) = output else {
        panic!("successful wait must return a payload, got {output:?}");
    };
    assert_eq!(payload.exit_code(), 0);
    let (doc, _) = payload.into_view();
    assert_eq!(doc["state"], "success");
    assert_eq!(doc["duration_ms"], 1000);
}

/// Both text and the structured payload expose the terminal state and its
/// diagnostic, so a script does not have to choose between them.
#[test]
fn wait_output_exposes_the_terminal_state_and_diagnostic() {
    let invoke = invoke_result(false);
    let entry = wait_entry("failed", Some("step_failed: implement\nblew up"));
    let lines = wait_lines(&invoke, &entry).join("\n");

    assert!(lines.contains("State: failed"), "{lines}");
    assert!(lines.contains("Finished: 2026-08-15T00:01:00Z"), "{lines}");
    assert!(
        lines.contains("Error: step_failed: implement blew up"),
        "a multi-line diagnostic must stay on one line: {lines}"
    );
    assert!(
        lines.contains("orbit run show jrun-20260815-0001"),
        "{lines}"
    );
}

/// A parent state carrying one blocking child dispatch [ORB-10971].
fn state_with_child_dispatch(
    run: &orbit_types::workflow::JobRun,
    phase: orbit_types::workflow::ChildDispatchPhase,
) -> PipelineState {
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.record_child_dispatch(
        orbit_types::workflow::ChildDispatch::submitted(
            "jrun-child-leaves".to_string(),
            "task_auto_pipeline".to_string(),
            "invoke_and_wait".to_string(),
            true,
            false,
            chrono::Utc::now(),
        )
        .with_parent_step_id(Some("ship_leaves".to_string())),
    );
    state.advance_child_dispatch("jrun-child-leaves", phase, None, None);
    state
}

#[test]
fn job_run_json_names_the_child_a_running_parent_dispatched() {
    let run = test_run(JobRunState::Running);
    let state = state_with_child_dispatch(&run, orbit_types::workflow::ChildDispatchPhase::Waiting);

    let value = cli_job_run_to_json(&run, Some(&state));

    let dispatches = value["child_dispatches"]
        .as_array()
        .expect("child_dispatches array");
    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0]["child_run_id"], json!("jrun-child-leaves"));
    assert_eq!(dispatches[0]["job_name"], json!("task_auto_pipeline"));
    assert_eq!(dispatches[0]["parent_step_id"], json!("ship_leaves"));
    assert_eq!(dispatches[0]["phase"], json!("waiting"));
}

#[test]
fn job_run_json_keeps_child_lineage_for_a_terminal_run() {
    // Unlike the waiting reasons above, lineage is not stale once the parent
    // stops: it is the only handle on the child the parent left behind.
    let run = test_run(JobRunState::Success);
    let state =
        state_with_child_dispatch(&run, orbit_types::workflow::ChildDispatchPhase::Terminal);

    let value = cli_job_run_to_json(&run, Some(&state));

    assert_eq!(value["waiting_on_deps"], Value::Null);
    assert_eq!(
        value["child_dispatches"][0]["child_run_id"],
        json!("jrun-child-leaves")
    );
}

#[test]
fn job_run_json_always_carries_a_child_dispatch_array() {
    let run = test_run(JobRunState::Running);

    let value = cli_job_run_to_json(&run, None);

    assert_eq!(
        value["child_dispatches"],
        json!([]),
        "readers must not have to distinguish 'no children' from 'field absent'"
    );
}
