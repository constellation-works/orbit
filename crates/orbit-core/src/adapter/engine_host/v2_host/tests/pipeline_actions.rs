use super::super::pipeline_actions::*;
use crate::OrbitRuntime;
use orbit_engine::DispatchError;
use serde_json::json;

fn action_failure_message(err: DispatchError, expected_action: &str) -> String {
    match err {
        DispatchError::DeterministicActionFailed { action, message } => {
            assert_eq!(action, expected_action);
            message
        }
        other => panic!("expected deterministic action failure, got {other}"),
    }
}

#[test]
fn pipeline_success_guard_accepts_succeeded_result() {
    let output = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({
            "result": {
                "run_id": "jrun-ok",
                "status": "succeeded"
            }
        }),
    )
    .expect("succeeded result should pass");

    assert_eq!(output["succeeded"], json!(true));
    assert_eq!(output["checked_count"], json!(1));
}

#[test]
fn pipeline_success_guard_rejects_failed_result() {
    let err = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({
            "context": "task gate child",
            "result": {
                "run_id": "jrun-failed",
                "status": "failed",
                "error": "implementation failed"
            }
        }),
    )
    .expect_err("failed child run should fail the guard");

    let message = action_failure_message(err, "pipeline_success_guard");
    assert!(message.contains("task gate child did not succeed"));
    assert!(message.contains("jrun-failed"));
    assert!(message.contains("status failed"));
    assert!(message.contains("implementation failed"));
}

#[test]
fn pipeline_success_guard_rejects_mixed_results() {
    let err = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({
            "results": [
                {
                    "run_id": "jrun-ok",
                    "status": "succeeded"
                },
                {
                    "run_id": "jrun-cancelled",
                    "status": "cancelled"
                },
                null
            ]
        }),
    )
    .expect_err("any non-succeeded result should fail the guard");

    let message = action_failure_message(err, "pipeline_success_guard");
    assert!(message.contains("results[1] run jrun-cancelled status cancelled"));
    assert!(message.contains("results[2] missing string status"));
}

#[test]
fn pipeline_success_guard_records_exact_terminal_results_and_counts() {
    let results = json!([
        {
            "run_id": "jrun-ok",
            "status": "succeeded"
        },
        {
            "run_id": "jrun-failed",
            "status": "failed",
            "error": "implementation failed"
        },
        {
            "run_id": "jrun-cancelled",
            "status": "cancelled",
            "error": "operator cancelled"
        },
        {
            "run_id": "jrun-interrupted",
            "status": "interrupted",
            "error": "worker disappeared"
        },
        {
            "run_id": "jrun-timeout",
            "status": "timeout",
            "error": "wait deadline elapsed"
        }
    ]);
    let output = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({
            "results": results,
            "allow_non_success": true
        }),
    )
    .expect("terminal child outcomes should be recorded");

    assert_eq!(output["succeeded"], false);
    assert_eq!(output["checked_count"], 5);
    assert_eq!(output["succeeded_count"], 1);
    assert_eq!(output["non_success_count"], 4);
    assert_eq!(output["results"], results);
}

#[test]
fn pipeline_success_guard_record_mode_rejects_malformed_or_non_terminal_results() {
    for (result, expected) in [
        (
            json!({"status": "failed", "error": "missing linkage"}),
            "missing non-empty string run_id",
        ),
        (
            json!({"run_id": "jrun-running", "status": "running"}),
            "has non-terminal status running",
        ),
        (
            json!({"run_id": "jrun-bad-error", "status": "failed", "error": 42}),
            "has non-string error",
        ),
    ] {
        let err = pipeline_success_guard(
            "pipeline_success_guard",
            &json!({
                "results": [result],
                "allow_non_success": true
            }),
        )
        .expect_err("malformed result must fail closed");
        let message = action_failure_message(err, "pipeline_success_guard");
        assert!(message.contains(expected), "{message}");
    }
}

#[test]
fn gate_starvation_fail_names_both_conflicting_files_and_unmet_dependencies() {
    // A dependency-starved gate previously reported an empty
    // `conflicting_files` list and named no blocker at all.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let message = action_failure_message(
        gate_starvation_fail(
            &runtime,
            "gate_starvation_fail",
            &json!({
                "task_ids": ["ORB-2"],
                "conflicts": [],
                "waiting_on_deps": ["ORB-1"],
                "max_wait_seconds": 3600,
            }),
        )
        .expect_err("starvation always fails the run"),
        "gate_starvation_fail",
    );

    assert!(message.contains("gate.starvation"), "{message}");
    assert!(message.contains("ORB-1"), "{message}");
    assert!(message.contains("waiting_on_deps"), "{message}");
}

#[test]
fn gate_starvation_fail_tolerates_a_missing_waiting_on_deps_input() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let message = action_failure_message(
        gate_starvation_fail(
            &runtime,
            "gate_starvation_fail",
            &json!({
                "task_ids": ["ORB-2"],
                "conflicts": [{ "file": "file:src/lib.rs", "held_by": "task", "held_by_id": "ORB-3" }],
            }),
        )
        .expect_err("starvation always fails the run"),
        "gate_starvation_fail",
    );

    assert!(message.contains("file:src/lib.rs"), "{message}");
    assert!(message.contains("waiting_on_deps=[]"), "{message}");
}

// ─── invoke_and_wait dispatch checkpoint [ORB-10971] ──────────────────────

use crate::adapter::engine_host::v2_host::child_dispatch::{
    CHILD_DISPATCH_AUDIT, CHILD_WAIT_AUDIT,
};
use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase, PipelineState,
};
use serde_json::Value;
use std::cell::RefCell;

const CHILD_RUN: &str = "jrun-child-leaves";

/// A persisted parent run, standing in for `workspace_auto_pipeline` at the
/// moment `ship_leaves` begins.
fn parent_runtime() -> (OrbitRuntime, String) {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(
            "workspace_auto_pipeline",
            1,
            chrono::Utc::now(),
            Some(json!({})),
            None,
        )
        .expect("insert parent run");
    let state = PipelineState::new(
        run.run_id.clone(),
        "workspace_auto_pipeline".to_string(),
        json!({}),
    );
    runtime
        .stores()
        .jobs()
        .write_run_state(&run.run_id, &state)
        .expect("seed parent run state");
    (runtime, run.run_id)
}

/// The reported negative fixture: capacity available, no dependency or lock
/// wait, healthy worker startup.
fn ship_leaves_input(parent_run_id: &str) -> Value {
    json!({
        "run_id": parent_run_id,
        "step_id": "ship_leaves",
        "job_name": "task_auto_pipeline",
        "run_input": { "task_ids": ["ORB-1", "ORB-2"] },
    })
}

fn healthy_invoke_output() -> Value {
    json!({
        "run_id": CHILD_RUN,
        "job_name": "task_auto_pipeline",
        "queued": false,
        "submitted_at": "2026-08-22T19:55:00Z",
    })
}

fn recorded_dispatches(runtime: &OrbitRuntime, parent_run_id: &str) -> Vec<ChildDispatch> {
    runtime
        .read_run_state(parent_run_id)
        .expect("read parent state")
        .map(|state| state.child_dispatches)
        .unwrap_or_default()
}

fn audit_payloads(runtime: &OrbitRuntime, command: &str) -> Vec<(AuditEventStatus, Value)> {
    runtime
        .list_audit_events(None, None, None, None, 100)
        .expect("list audit events")
        .into_iter()
        .filter(|event| event.command == command)
        .map(|event| {
            let payload = event
                .arguments_json
                .as_deref()
                .map(|raw| serde_json::from_str::<Value>(raw).expect("audit payload json"))
                .unwrap_or(Value::Null);
            (event.status, payload)
        })
        .collect()
}

#[test]
fn the_child_run_is_durable_and_linked_before_the_wait_begins() {
    let (runtime, parent) = parent_runtime();
    // Captured from inside the wait: exactly what a concurrent CLI, MCP, API,
    // or dashboard reader would have seen while the parent was still blocked.
    let observed_mid_wait = RefCell::new(Vec::new());

    let output = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| Ok(healthy_invoke_output()),
        |args| {
            observed_mid_wait.replace(recorded_dispatches(&runtime, &parent));
            assert_eq!(args["run_ids"], json!([CHILD_RUN]));
            Ok(json!({
                "results": [{ "run_id": CHILD_RUN, "status": "succeeded" }],
            }))
        },
    )
    .expect("healthy dispatch succeeds");

    let mid_wait = observed_mid_wait.into_inner();
    assert_eq!(mid_wait.len(), 1, "child must be linked before the wait");
    let linked = &mid_wait[0];
    assert_eq!(linked.child_run_id, CHILD_RUN);
    assert_eq!(linked.job_name, "task_auto_pipeline");
    assert_eq!(linked.parent_step_id.as_deref(), Some("ship_leaves"));
    assert_eq!(linked.phase, ChildDispatchPhase::Waiting);
    assert!(linked.blocking);
    assert!(!linked.queued);

    // The blocking leaf contract: the wait entry is still what reaches
    // `pipeline_success_guard`.
    assert_eq!(output["run_id"], json!(CHILD_RUN));
    assert_eq!(output["status"], json!("succeeded"));
    pipeline_success_guard(
        "pipeline_success_guard",
        &json!({ "context": "workspace auto leaf ship", "result": output }),
    )
    .expect("the child's terminal status reaches the guard unchanged");

    let settled = recorded_dispatches(&runtime, &parent);
    assert_eq!(settled[0].phase, ChildDispatchPhase::Terminal);
    assert_eq!(settled[0].child_status.as_deref(), Some("succeeded"));

    let dispatch_audits = audit_payloads(&runtime, CHILD_DISPATCH_AUDIT);
    assert_eq!(dispatch_audits.len(), 1);
    assert_eq!(dispatch_audits[0].0, AuditEventStatus::Success);
    assert_eq!(dispatch_audits[0].1["child_run_id"], json!(CHILD_RUN));
    assert_eq!(dispatch_audits[0].1["parent_run_id"], json!(parent));
    assert_eq!(dispatch_audits[0].1["parent_step_id"], json!("ship_leaves"));

    let wait_audits = audit_payloads(&runtime, CHILD_WAIT_AUDIT);
    assert_eq!(wait_audits.len(), 1);
    assert_eq!(wait_audits[0].1["status"], json!("succeeded"));
}

#[test]
fn a_dispatch_that_never_produced_a_child_fails_instead_of_waiting() {
    let (runtime, parent) = parent_runtime();
    let waited = RefCell::new(false);

    let err = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| {
            Err(OrbitError::Execution(
                "pipeline worker for run 'jrun-x' could not start".to_string(),
            ))
        },
        |_| {
            waited.replace(true);
            Ok(json!({ "results": [] }))
        },
    )
    .expect_err("a failed submission must terminalize the step");

    assert!(
        !waited.into_inner(),
        "the step must never reach the one-hour wait without a durable child"
    );
    let message = action_failure_message(err, "invoke_and_wait");
    assert!(message.contains("pipeline.invoke failed"), "{message}");
    assert!(message.contains("could not start"), "{message}");

    assert!(
        recorded_dispatches(&runtime, &parent).is_empty(),
        "no child run id exists, so nothing may be linked"
    );
    let audits = audit_payloads(&runtime, CHILD_DISPATCH_AUDIT);
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].0, AuditEventStatus::Failure);
    assert!(
        audits[0].1["error"]
            .as_str()
            .expect("error text")
            .contains("could not start"),
        "the concrete invocation error is the diagnosis"
    );
}

#[test]
fn an_invoke_that_returns_no_run_id_is_treated_as_a_failed_dispatch() {
    let (runtime, parent) = parent_runtime();
    let waited = RefCell::new(false);

    let err = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| Ok(json!({ "queued": true })),
        |_| {
            waited.replace(true);
            Ok(json!({ "results": [] }))
        },
    )
    .expect_err("a run id is the only acceptable proof of a durable child");

    assert!(!waited.into_inner());
    let message = action_failure_message(err, "invoke_and_wait");
    assert!(message.contains("returned no run_id"), "{message}");
    assert_eq!(audit_payloads(&runtime, CHILD_DISPATCH_AUDIT).len(), 1);
}

#[test]
fn a_failed_child_stays_linked_with_its_terminal_status() {
    let (runtime, parent) = parent_runtime();

    let output = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| Ok(healthy_invoke_output()),
        |_| {
            Ok(json!({
                "results": [{
                    "run_id": CHILD_RUN,
                    "status": "failed",
                    "error": "implement_one exhausted retries",
                }],
            }))
        },
    )
    .expect("a failed child is an observed outcome, not an action failure");

    let dispatches = recorded_dispatches(&runtime, &parent);
    assert_eq!(dispatches[0].phase, ChildDispatchPhase::Terminal);
    assert_eq!(dispatches[0].child_status.as_deref(), Some("failed"));
    assert_eq!(
        dispatches[0].error.as_deref(),
        Some("implement_one exhausted retries")
    );

    pipeline_success_guard(
        "pipeline_success_guard",
        &json!({ "context": "workspace auto leaf ship", "result": output }),
    )
    .expect_err("the guard, not this action, decides the parent's fate");

    assert_eq!(
        audit_payloads(&runtime, CHILD_WAIT_AUDIT)[0].0,
        AuditEventStatus::Failure
    );
}

#[test]
fn mixed_crew_child_failure_closes_dispatch_and_fails_parent_guard_promptly() {
    let (runtime, parent) = parent_runtime();
    let concrete_error = "invalid input: task bundle mixes crews terra and sol; split the bundle or assign one crew instead of inheriting workflow.default_crew";
    let started = std::time::Instant::now();

    let output = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| Ok(healthy_invoke_output()),
        |_| {
            Ok(json!({
                "results": [{
                    "run_id": CHILD_RUN,
                    "status": "failed",
                    "finished_at": "2026-08-22T23:00:00Z",
                    "error": concrete_error,
                }],
            }))
        },
    )
    .expect("terminal child failure is returned as data");

    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    let dispatch = &recorded_dispatches(&runtime, &parent)[0];
    assert_eq!(dispatch.phase, ChildDispatchPhase::Terminal);
    assert_eq!(dispatch.child_status.as_deref(), Some("failed"));
    assert_eq!(dispatch.error.as_deref(), Some(concrete_error));

    let error = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({ "context": "workspace auto leaf ship", "results": [output] }),
    )
    .expect_err("the parent must fail on the terminal mixed-crew child");
    let message = action_failure_message(error, "pipeline_success_guard");
    assert!(message.contains("mixes crews"), "{message}");
}

#[test]
fn a_wait_that_errors_leaves_the_child_linked_without_claiming_it_failed() {
    let (runtime, parent) = parent_runtime();

    let err = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| Ok(healthy_invoke_output()),
        |_| Err(OrbitError::Execution("store unavailable".to_string())),
    )
    .expect_err("an unobservable wait fails the step");

    let message = action_failure_message(err, "invoke_and_wait");
    assert!(message.contains("pipeline.wait failed"), "{message}");

    let dispatches = recorded_dispatches(&runtime, &parent);
    assert_eq!(dispatches[0].child_run_id, CHILD_RUN);
    assert_eq!(dispatches[0].phase, ChildDispatchPhase::Terminal);
    assert_eq!(
        dispatches[0].child_status, None,
        "the parent never observed a child status, so it must not invent one"
    );
    assert_eq!(
        audit_payloads(&runtime, CHILD_WAIT_AUDIT)[0].1["status"],
        json!("unobserved")
    );
}

#[test]
fn a_detached_child_is_recorded_as_non_blocking() {
    let (runtime, parent) = parent_runtime();
    let mut state = runtime
        .read_run_state(&parent)
        .expect("read state")
        .expect("state");
    state.record_child_dispatch(ChildDispatch::submitted(
        "jrun-child-epic".to_string(),
        "epic_pipeline".to_string(),
        "invoke_detached".to_string(),
        false,
        false,
        chrono::Utc::now(),
    ));
    runtime
        .stores()
        .jobs()
        .write_run_state(&parent, &state)
        .expect("write state");

    let dispatches = recorded_dispatches(&runtime, &parent);
    assert_eq!(dispatches.len(), 1);
    assert!(!dispatches[0].blocking);
    assert_eq!(
        dispatches[0].cancellation_policy(),
        ChildCancellationPolicy::Detach,
        "a detached child was dispatched to outlive its parent's step"
    );
}

#[test]
fn invoke_detached_skips_when_the_parent_has_stopped_admissions() {
    let (runtime, parent) = parent_runtime();
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&parent, chrono::Utc::now(), std::process::id())
        .expect("start parent");
    runtime
        .stop_workspace_auto_admissions(crate::application::job::DrainAdmissionsStopRequest {
            actor: "tester",
            source: "unit",
            reason: None,
            claim_token: None,
        })
        .expect("stop parent");

    let output = invoke_detached(
        &runtime,
        "invoke_detached",
        &ship_leaves_input(&parent),
        orbit_tools::ToolContext::default(),
    )
    .expect("stopped parent skips rather than failing");

    assert_eq!(output["skipped"], true);
    assert_eq!(output["reason"], "admissions_stopped");
    assert!(output.get("run_id").is_none());
    assert!(
        recorded_dispatches(&runtime, &parent).is_empty(),
        "a skipped invoke must not create a child"
    );
}
