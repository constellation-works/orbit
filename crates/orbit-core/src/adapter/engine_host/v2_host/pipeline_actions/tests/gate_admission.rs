use super::super::invoke::invoke_and_wait_with;
use super::super::*;
use crate::OrbitRuntime;
use serde_json::json;

use super::results::action_failure_message;
use orbit_types::workflow::JobRunState;
use serde_json::Value;

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

use super::invoke::{
    CHILD_RUN, audit_payloads, healthy_invoke_output, parent_runtime, recorded_dispatches,
    ship_leaves_input,
};
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
