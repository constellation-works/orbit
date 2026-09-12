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
fn pipeline_success_guard_accepts_success_result() {
    let output = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({
            "result": {
                "run_id": "jrun-ok",
                "status": "success"
            }
        }),
    )
    .expect("canonical success spelling should pass");

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
fn pipeline_success_guard_reports_task_pilot_apply_without_unknown_run() {
    let error = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({
            "context": "task-pilot apply",
            "result": {
                "status": "failed",
                "error": "partition 0, task ORB-11991: selector missing",
                "partition_decisions": [],
                "applied_count": 4,
                "unresolved_count": 1,
            }
        }),
    )
    .expect_err("unresolved apply fails the guard");

    let message = action_failure_message(error, "pipeline_success_guard");
    assert!(message.contains("partition 0, task ORB-11991: selector missing"));
    assert!(message.contains("4 applied, 1 unresolved"));
    assert!(!message.contains("<unknown>"));
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
use orbit_store::contracts::{ChildJobRunAdmissionOutcome, ChildJobRunAdmissionParams};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{
    ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase, JobRunState, PipelineState,
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

fn atomic_child_admission(parent_run_id: &str, blocking: bool) -> ChildJobRunAdmissionParams {
    ChildJobRunAdmissionParams {
        parent_run_id: parent_run_id.to_string(),
        parent_step_id: Some("ship_leaves".to_string()),
        job_id: "task_auto_pipeline".to_string(),
        action: if blocking {
            "invoke_and_wait".to_string()
        } else {
            "invoke_detached".to_string()
        },
        blocking,
        attempt: 1,
        scheduled_at: chrono::Utc::now(),
        input: Some(json!({ "task_ids": ["ORB-1", "ORB-2"] })),
        authority: None,
    }
}

fn admitted_output(outcome: ChildJobRunAdmissionOutcome) -> Value {
    match outcome {
        ChildJobRunAdmissionOutcome::Admitted(run) => json!({
            "run_id": run.run_id,
            "job_name": run.job_id,
            "queued": false,
            "submitted_at": run.scheduled_at.to_rfc3339(),
        }),
        ChildJobRunAdmissionOutcome::AdmissionsStopped => json!({
            "skipped": true,
            "reason": "admissions_stopped",
            "job_name": "task_auto_pipeline",
        }),
        ChildJobRunAdmissionOutcome::Refused { reason } => json!({
            "skipped": true,
            "reason": reason,
            "job_name": "task_auto_pipeline",
        }),
    }
}

/// The exact TOCTOU sequence from ORB-11310: the action observes eligibility,
/// stop acknowledges, and only then does the stale action attempt its durable
/// admission. The store boundary, rather than another read in the action,
/// refuses the child.
#[test]
fn stop_between_eligibility_observation_and_admission_creates_no_child() {
    let (runtime, parent) = parent_runtime();
    assert!(!runtime.drain_admissions_stopped(&parent));

    let output = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| {
            runtime
                .stop_workspace_auto_admissions(
                    crate::application::job::DrainAdmissionsStopRequest {
                        actor: "tester",
                        source: "unit",
                        reason: None,
                        claim_token: None,
                    },
                )
                .expect("stop acknowledges between observation and admission");
            runtime
                .stores()
                .jobs()
                .admit_child_job_run(&atomic_child_admission(&parent, true))
                .map(admitted_output)
        },
        |_| panic!("a stopped admission must not wait on a child"),
    )
    .expect("admissions stop is an idempotent skip");

    assert_eq!(output["skipped"], true);
    assert_eq!(output["status"], JobRunState::Success.to_string());
    assert_eq!(output["reason"], "admissions_stopped");
    assert!(
        runtime
            .stores()
            .jobs()
            .list_job_runs("task_auto_pipeline")
            .expect("list children")
            .is_empty()
    );
}

/// Admission's SQLite transaction ends before the action waits. A stop issued
/// from inside the wait callback therefore completes immediately, while the
/// already-admitted child remains linked and reaches its ordinary result.
#[test]
fn blocking_child_wait_holds_no_admission_lock_and_stop_does_not_cancel_child() {
    let (runtime, parent) = parent_runtime();
    let output = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| {
            runtime
                .stores()
                .jobs()
                .admit_child_job_run(&atomic_child_admission(&parent, true))
                .map(admitted_output)
        },
        |args| {
            runtime
                .stop_workspace_auto_admissions(
                    crate::application::job::DrainAdmissionsStopRequest {
                        actor: "tester",
                        source: "unit",
                        reason: None,
                        claim_token: None,
                    },
                )
                .expect("stop must not deadlock behind child wait");
            let child_run_id = args["run_ids"][0].as_str().expect("child run id");
            Ok(json!({
                "results": [{ "run_id": child_run_id, "status": "succeeded" }]
            }))
        },
    )
    .expect("already-admitted child completes normally");

    assert_eq!(output["status"], "succeeded");
    let parent_state = runtime
        .read_run_state(&parent)
        .expect("read parent")
        .expect("parent state");
    assert!(parent_state.admissions_stopped());
    assert_eq!(parent_state.child_dispatches.len(), 1);
    let child = runtime
        .show_job_run(&parent_state.child_dispatches[0].child_run_id)
        .expect("read child");
    assert_eq!(child.state, orbit_types::workflow::JobRunState::Pending);
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

// ---------------------------------------------------------------------------
// [ORB-11305] Live eligibility re-check at the child-dispatch boundary.
//
// The incident this pins: a bundle was admitted while its task was `backlog`,
// its gate then sat in `wait_for_window` waiting on locks held by another run,
// a human withdrew the task (backlog -> proposed) and archived it, and when the
// locks freed the gate dispatched anyway on its hour-old admission snapshot.
// `worktree_setup` moved the archived task to `in-progress` and launched a
// provider against work its owner had explicitly withdrawn.
// ---------------------------------------------------------------------------

use crate::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::TaskStatus;

const GATE_ADMISSION_STOP_AUDIT: &str = "gate.withdrawn";
const GATE_STALE_NOOP_AUDIT: &str = "gate.stale_noop";

/// A gate `dispatch_child` input carrying the admission re-check contract that
/// `task_gate_pipeline` passes.
fn gate_dispatch_input(parent_run_id: &str, task_ids: &[&str]) -> Value {
    json!({
        "run_id": parent_run_id,
        "step_id": "dispatch_child",
        "job_name": "task_pr_pipeline",
        "run_input": { "task_ids": task_ids },
        "admission_task_ids": task_ids,
        "admission_workflow": "worktree_setup",
    })
}

fn backlog_task(runtime: &OrbitRuntime, title: &str) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Admitted while backlog.".to_string(),
            ..Default::default()
        })
        .expect("create task");
    runtime
        .approve_task(&task.id, None, None)
        .expect("approve into backlog");
    task.id
}

/// Park a task the way a human would, through whichever public transition owns
/// that status — the domain refuses several of them as bare status writes.
fn park_task(runtime: &OrbitRuntime, task_id: &str, status: TaskStatus) {
    match status {
        TaskStatus::Archived => {
            runtime.archive_task(task_id).expect("archive task");
        }
        TaskStatus::Rejected => {
            runtime
                .reject_task(task_id, "withdrawn by its owner".to_string(), None)
                .expect("reject task");
        }
        other => {
            runtime
                .update_task(
                    task_id,
                    TaskUpdateParams {
                        status: Some(other),
                        ..Default::default()
                    },
                )
                .expect("apply status change");
        }
    }
}

/// Walk a task to `review` the way its pipeline would, execution summary and
/// all, so the "already shipped" branch is reached through a real transition.
fn ship_to_review(runtime: &OrbitRuntime, task_id: &str) {
    park_task(runtime, task_id, TaskStatus::InProgress);
    runtime
        .update_task(
            task_id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                execution_summary: Some("shipped while the gate waited".to_string()),
                ..Default::default()
            },
        )
        .expect("move task to review");
}

/// Drive `invoke_and_wait` with an invoke that panics if it is ever reached, so
/// "no child was dispatched" is proven rather than inferred from state.
fn dispatch_expecting_no_child(runtime: &OrbitRuntime, input: &Value) -> Value {
    invoke_and_wait_with(
        runtime,
        "invoke_and_wait",
        input,
        |_| panic!("an ineligible bundle must not submit a child run"),
        |_| panic!("an ineligible bundle must not wait on a child run"),
    )
    .expect("an admission stop is a result, not an activity failure")
}

/// The whole incident, at the seam that decides it.
#[test]
fn a_withdrawn_task_is_refused_at_dispatch_after_the_gate_waited() {
    let (runtime, parent) = parent_runtime();
    let task_id = backlog_task(&runtime, "Hermes work the owner withdrew");

    // The gate is admitted here: the task is backlog, so this bundle would have
    // dispatched had it not had to wait.
    assert!(
        runtime
            .ensure_task_can_enter_workflow_as_system(&task_id, "worktree_setup")
            .is_ok(),
        "the bundle must be genuinely admissible at admission time"
    );

    // ... the gate waits on locks, and during that wait the human withdraws the
    // task from the backlog and then archives it.
    park_task(&runtime, &task_id, TaskStatus::Proposed);
    park_task(&runtime, &task_id, TaskStatus::Archived);

    // ... the locks free and the gate wakes up with its stale snapshot.
    let output = dispatch_expecting_no_child(&runtime, &gate_dispatch_input(&parent, &[&task_id]));

    assert_eq!(output["skipped"], json!(true));
    assert_eq!(
        output["status"], "failed",
        "a withdrawal must not be reported as a successful bundle"
    );
    let reason = output["reason"].as_str().expect("reason");
    assert!(reason.contains(&task_id), "reason must name the task");
    assert!(reason.contains("archived"), "reason must name the status");
    assert!(
        reason.contains("backlog"),
        "reason must name the remedy: {reason}"
    );
    // `pipeline_success_guard` quotes `error`, so the operator sees the reason
    // on the failing gate step and not only in the audit log.
    assert_eq!(output["error"], output["reason"]);
    assert_eq!(output["task_statuses"][0]["task_id"], json!(task_id));
    assert_eq!(output["task_statuses"][0]["status"], json!("archived"));
    assert_eq!(output["task_statuses"][0]["admissible"], json!(false));

    // The task is untouched: no archived -> in-progress mutation, no coupling.
    let after = runtime.get_task(&task_id).expect("reload task");
    assert_eq!(after.status, TaskStatus::Archived);
    assert_eq!(after.job_run_id, None);
    assert!(
        recorded_dispatches(&runtime, &parent).is_empty(),
        "no child run may be linked to the parent"
    );

    let audits = audit_payloads(&runtime, GATE_ADMISSION_STOP_AUDIT);
    assert_eq!(audits.len(), 1, "the stop must be explainable from audit");
    assert_eq!(audits[0].1["outcome"], json!("withdrawn"));
    assert_eq!(audits[0].1["task_ids"], json!([task_id]));
}

/// The synthetic result must flow through the gate's own YAML the way a real
/// child result does: non-success, so `release_reservation` runs first and
/// `require_child_success` then fails the run with the reason attached.
#[test]
fn a_withdrawn_dispatch_result_releases_the_reservation_then_fails_the_gate() {
    let (runtime, parent) = parent_runtime();
    let task_id = backlog_task(&runtime, "Withdrawn mid-wait");
    park_task(&runtime, &task_id, TaskStatus::Archived);

    let output = dispatch_expecting_no_child(&runtime, &gate_dispatch_input(&parent, &[&task_id]));

    // `release_reservation` guards on `status` being none of these.
    let status = output["status"].as_str().expect("status");
    assert!(
        !matches!(status, "timeout" | "pending" | "running"),
        "the gate must consider the wait terminal so the reservation is released"
    );

    let err = pipeline_success_guard(
        "pipeline_success_guard",
        &json!({
            "context": "task_gate_pipeline child run",
            "result": output,
        }),
    )
    .expect_err("an ineligible bundle must fail the gate");
    let message = action_failure_message(err, "pipeline_success_guard");
    assert!(message.contains("task_gate_pipeline child run did not succeed"));
    assert!(message.contains(&task_id));
    assert!(message.contains("no longer admissible"));
}

/// Every status a human parks work in is refused, not just `archived`.
#[test]
fn each_withdrawn_status_is_refused_at_the_dispatch_boundary() {
    let (runtime, parent) = parent_runtime();

    for status in [
        TaskStatus::Proposed,
        TaskStatus::Someday,
        TaskStatus::Archived,
        TaskStatus::Rejected,
        TaskStatus::Blocked,
    ] {
        let task_id = backlog_task(&runtime, &format!("Parked in {status}"));
        park_task(&runtime, &task_id, status);

        let output =
            dispatch_expecting_no_child(&runtime, &gate_dispatch_input(&parent, &[&task_id]));
        assert_eq!(output["status"], "failed", "{status} must refuse dispatch");
        assert_eq!(
            runtime.get_task(&task_id).expect("reload").status,
            status,
            "{status} must survive the refusal unchanged"
        );
    }
}

/// A bundle that mixes already-shipped work with a withdrawal must not report
/// the whole bundle as a successful no-op — the withdrawal is the stronger
/// signal and has to reach the operator.
#[test]
fn a_withdrawal_outranks_a_stale_noop_in_the_same_bundle() {
    let (runtime, parent) = parent_runtime();
    let shipped = backlog_task(&runtime, "Already in review");
    ship_to_review(&runtime, &shipped);
    let withdrawn = backlog_task(&runtime, "Withdrawn by its owner");
    park_task(&runtime, &withdrawn, TaskStatus::Archived);

    let output = dispatch_expecting_no_child(
        &runtime,
        &gate_dispatch_input(&parent, &[&shipped, &withdrawn]),
    );

    assert_eq!(output["status"], "failed");
    let reason = output["reason"].as_str().expect("reason");
    assert!(reason.contains(&withdrawn));
    assert!(
        !reason.contains(&shipped),
        "the shipped task is not why this bundle stopped: {reason}"
    );
}

/// Positive control: an eligible bundle still dispatches normally after the
/// gate waited. The re-check must not cost a healthy run its dispatch.
#[test]
fn an_eligible_bundle_still_dispatches_after_the_gate_waited() {
    let (runtime, parent) = parent_runtime();
    let backlog = backlog_task(&runtime, "Still wanted after the wait");
    let retried = backlog_task(&runtime, "This run's own retry");
    park_task(&runtime, &retried, TaskStatus::InProgress);

    let output = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &gate_dispatch_input(&parent, &[&backlog, &retried]),
        |_| Ok(json!({ "run_id": CHILD_RUN, "job_name": "task_pr_pipeline", "queued": false })),
        |_| Ok(json!({ "results": [{ "run_id": CHILD_RUN, "status": "succeeded" }] })),
    )
    .expect("an eligible bundle dispatches");

    assert_eq!(output["status"], "succeeded");
    assert!(
        output.get("skipped").is_none(),
        "a real dispatch is not a skip"
    );
    assert_eq!(recorded_dispatches(&runtime, &parent).len(), 1);
}

/// Positive control: already-shipped work keeps its successful no-op. Making
/// withdrawal fail the gate must not turn "this already landed" into a failure.
#[test]
fn already_shipped_work_still_reports_a_succeeded_noop() {
    let (runtime, parent) = parent_runtime();
    let task_id = backlog_task(&runtime, "Landed while the gate waited");
    ship_to_review(&runtime, &task_id);

    let output = dispatch_expecting_no_child(&runtime, &gate_dispatch_input(&parent, &[&task_id]));

    assert_eq!(output["status"], JobRunState::Success.to_string());
    assert_eq!(output["skipped"], json!(true));
    assert!(
        output.get("error").is_none(),
        "a successful stop must not carry error: {output}"
    );
    assert!(
        pipeline_success_guard(
            "pipeline_success_guard",
            &json!({ "result": output.clone() })
        )
        .is_ok(),
        "a stale no-op must still pass the gate's success guard"
    );
    assert_eq!(audit_payloads(&runtime, GATE_STALE_NOOP_AUDIT).len(), 1);
}

/// [ORB-12299] Synthetic skip / stale-noop wait results must use a status the
/// published `invoke_and_wait` enum actually declares, not the compatibility
/// token `succeeded`.
#[test]
fn synthetic_skip_and_admission_stop_status_is_in_invoke_and_wait_enum() {
    let statuses = published_invoke_and_wait_status_enum();
    let canonical = JobRunState::Success.to_string();
    assert!(
        statuses.iter().any(|status| status == &canonical),
        "published wait enum must contain {canonical}, got {statuses:?}"
    );
    assert!(
        !statuses.iter().any(|status| status == "succeeded"),
        "succeeded is a compatibility token, not a published wait status: {statuses:?}"
    );

    let (runtime, parent) = parent_runtime();
    let skip = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| {
            Ok(json!({
                "skipped": true,
                "reason": "admissions_stopped",
                "job_name": "task_auto_pipeline",
            }))
        },
        |_| panic!("a skipped invoke must not wait on a child"),
    )
    .expect("admissions skip is a wait result");
    let skip_status = skip["status"].as_str().expect("skip status");
    assert!(
        statuses.iter().any(|status| status == skip_status),
        "skip status {skip_status:?} is not in the published enum {statuses:?}"
    );
    assert_eq!(skip_status, canonical);

    let task_id = backlog_task(&runtime, "Landed while the gate waited");
    ship_to_review(&runtime, &task_id);
    let stop = dispatch_expecting_no_child(&runtime, &gate_dispatch_input(&parent, &[&task_id]));
    let stop_status = stop["status"].as_str().expect("admission-stop status");
    assert!(
        statuses.iter().any(|status| status == stop_status),
        "admission-stop status {stop_status:?} is not in the published enum {statuses:?}"
    );
    assert_eq!(stop_status, canonical);
    assert!(
        stop.get("error").is_none(),
        "a successful admission stop must not carry error: {stop}"
    );
}

fn published_invoke_and_wait_status_enum() -> Vec<String> {
    use orbit_engine::activity_job::load_activity_asset;

    let (_, yaml) = crate::runtime::assets::DEFAULT_ACTIVITY_FILES
        .iter()
        .find(|(name, _)| *name == "invoke_and_wait")
        .expect("invoke_and_wait activity is seeded");
    let wait = load_activity_asset(yaml).expect("parse invoke_and_wait");
    wait.spec.output_schema_json["properties"]["status"]["enum"]
        .as_array()
        .expect("invoke_and_wait status enum")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("status enum values are strings")
                .to_string()
        })
        .collect()
}

/// A task id that resolves to no task at all stays a hard activity failure:
/// that is a malformed bundle, not a lifecycle decision, and silently
/// succeeding a gate over it would hide the misconfiguration.
#[test]
fn an_unresolvable_admission_task_still_fails_the_activity() {
    let (runtime, parent) = parent_runtime();

    let err = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &gate_dispatch_input(&parent, &["ORB-99999"]),
        |_| panic!("must not dispatch on an unresolvable bundle"),
        |_| panic!("must not wait on an unresolvable bundle"),
    )
    .expect_err("an unknown task id is a hard failure");
    let message = action_failure_message(err, "invoke_and_wait");
    assert!(message.contains("workflow admission check before child dispatch failed"));
}

/// Without the admission contract the activity is unchanged: callers that pass
/// no `admission_task_ids` (every non-gate parent) get no re-check.
#[test]
fn a_dispatch_without_admission_task_ids_is_not_rechecked() {
    let (runtime, parent) = parent_runtime();

    let output = invoke_and_wait_with(
        &runtime,
        "invoke_and_wait",
        &ship_leaves_input(&parent),
        |_| Ok(healthy_invoke_output()),
        |_| Ok(json!({ "results": [{ "run_id": CHILD_RUN, "status": "succeeded" }] })),
    )
    .expect("no admission contract, no re-check");

    assert_eq!(output["status"], "succeeded");
    assert_eq!(recorded_dispatches(&runtime, &parent).len(), 1);
}
