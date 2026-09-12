//! Regression coverage for the shared job-run JSON projection.

use chrono::Utc;
use orbit_types::workflow::{
    ChildDispatch, ChildDispatchPhase, JobRun, JobRunState, JobRunStep, JobRunTrigger,
    JobTargetType, PipelineState,
};
use serde_json::{Value, json};

use super::super::projection::{
    ActivityInvocationEvidence, job_run_to_json, job_run_to_json_with_activity_provenance,
};

fn test_run(state: JobRunState) -> JobRun {
    let now = Utc::now();
    JobRun {
        run_id: "jrun-projection".to_string(),
        job_id: "task_auto_pipeline".to_string(),
        attempt: 1,
        state,
        scheduled_at: now,
        started_at: Some(now),
        finished_at: None,
        duration_ms: None,
        created_at: now,
        pid: Some(42),
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    }
}

fn state_with_child_dispatch(run: &JobRun) -> PipelineState {
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.set_waiting_reasons(
        Some(vec!["ORB-1".to_string()]),
        Some(vec!["file:src/lib.rs".to_string()]),
    );
    state.record_child_dispatch(
        ChildDispatch::submitted(
            "jrun-child".to_string(),
            "task_auto_pipeline".to_string(),
            "invoke_and_wait".to_string(),
            true,
            false,
            Utc::now(),
        )
        .with_parent_step_id(Some("ship".to_string())),
    );
    state.advance_child_dispatch("jrun-child", ChildDispatchPhase::Waiting, None, None);
    state
}

#[test]
fn active_run_projects_waiting_reasons_and_child_dispatches() {
    let run = test_run(JobRunState::Running);
    let state = state_with_child_dispatch(&run);

    let value = job_run_to_json(&run, Some(&state));

    assert_eq!(value["waiting_on_deps"], json!(["ORB-1"]));
    assert_eq!(value["waiting_on_locks"], json!(["file:src/lib.rs"]));
    assert_eq!(
        value["child_dispatches"][0]["child_run_id"],
        json!("jrun-child")
    );
    assert_eq!(value["drain_admissions_stop"], Value::Null);
}

#[test]
fn projection_includes_routine_trigger_on_a_terminal_run() {
    let run = test_run(JobRunState::Success);
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    state.trigger = Some(JobRunTrigger::routine(
        "auto-task-scheduler-nebula",
        "2026-09-12T03:00:00Z",
    ));

    let value = job_run_to_json(&run, Some(&state));

    assert_eq!(value["trigger"]["kind"], json!("routine"));
    assert_eq!(
        value["trigger"]["routine"],
        json!("auto-task-scheduler-nebula")
    );
    assert_eq!(value["trigger"]["slot"], json!("2026-09-12T03:00:00Z"));
}

#[test]
fn terminal_run_keeps_child_lineage_but_drops_waiting_reasons() {
    let run = test_run(JobRunState::Cancelled);
    let state = state_with_child_dispatch(&run);

    let value = job_run_to_json(&run, Some(&state));

    assert_eq!(value["waiting_on_deps"], Value::Null);
    assert_eq!(value["waiting_on_locks"], Value::Null);
    assert_eq!(
        value["child_dispatches"][0]["child_run_id"],
        json!("jrun-child")
    );
}

#[test]
fn projection_separates_requested_resolved_and_actual_activity_identity() {
    let mut run = test_run(JobRunState::Success);
    run.input = Some(json!({ "crew": "luna" }));
    run.resolved_crew = Some("luna".to_string());
    run.crew_model = Some("gpt-5.6-luna".to_string());
    run.steps = vec![
        JobRunStep {
            step_index: 0,
            target_type: JobTargetType::Activity,
            target_id: "system".to_string(),
            state: JobRunState::Success,
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            duration_ms: Some(1),
            exit_code: Some(0),
            agent_response_json: None,
            error_code: None,
            error_message: None,
        },
        JobRunStep {
            step_index: 1,
            target_type: JobTargetType::Activity,
            target_id: "implement".to_string(),
            state: JobRunState::Success,
            started_at: Some(Utc::now()),
            finished_at: Some(Utc::now()),
            duration_ms: Some(1),
            exit_code: Some(0),
            agent_response_json: None,
            error_code: None,
            error_message: None,
        },
        JobRunStep {
            step_index: 2,
            target_type: JobTargetType::Activity,
            target_id: "queued".to_string(),
            state: JobRunState::Pending,
            started_at: None,
            finished_at: None,
            duration_ms: None,
            exit_code: None,
            agent_response_json: None,
            error_code: None,
            error_message: None,
        },
    ];

    let value = job_run_to_json_with_activity_provenance(
        &run,
        None,
        &[
            ActivityInvocationEvidence {
                activity_id: "system".to_string(),
                provider: "claude".to_string(),
                model: Some("fable".to_string()),
            },
            ActivityInvocationEvidence {
                activity_id: "implement".to_string(),
                provider: "codex".to_string(),
                model: Some("gpt-5.6-luna".to_string()),
            },
        ],
    );

    assert_eq!(value["requested_crew"], "luna");
    assert_eq!(value["resolved_run_crew"]["model"], "gpt-5.6-luna");
    assert_eq!(value["activity_provenance"][0]["actual_status"], "recorded");
    assert_eq!(
        value["activity_provenance"][0]["invocations"][0]["provider"],
        "claude"
    );
    assert_eq!(
        value["activity_provenance"][0]["invocations"][0]["model"],
        "fable"
    );
    assert_eq!(
        value["activity_provenance"][1]["invocations"][0]["provider"],
        "codex"
    );
    assert_eq!(
        value["activity_provenance"][2]["actual_status"],
        "not_started"
    );
}
