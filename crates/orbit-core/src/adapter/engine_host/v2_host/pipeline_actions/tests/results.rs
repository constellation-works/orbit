use super::super::*;
use orbit_engine::DispatchError;
use serde_json::json;

pub(super) fn action_failure_message(err: DispatchError, expected_action: &str) -> String {
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
