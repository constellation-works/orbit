//! Filing cancelled zero-step CI jobs as inconclusive, never as repair tasks.

use serde_json::{Value, json};

use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;

use super::ci_failure_tasks::{CHECKOUT, failure, file, file_error, snapshot};

const CODEQL_RUN: u64 = 34_168_609_753;
const CODEQL_JOBS: [u64; 4] = [
    101_884_485_004,
    101_884_485_211,
    101_884_485_221,
    101_884_485_257,
];

fn cancelled_zero_step(job_id: u64, name: &str) -> Value {
    json!({
        "run_id": CODEQL_RUN,
        "job_id": job_id,
        "workflow": "CodeQL",
        "title": "CodeQL",
        "status": "completed",
        "conclusion": "cancelled",
        "event": "push",
        "url": format!("https://github.com/acme/orbit/actions/runs/{CODEQL_RUN}"),
        "created_at": "2026-09-07T23:00:00Z",
        "head_branch": "agent-main",
        "ref_kind": "integration",
        "investigated": true,
        "evidence_state": "inconclusive",
        "inconclusive_reason": "cancelled_without_failed_steps",
        "failed_jobs": [{
            "job_id": job_id,
            "name": name,
            "conclusion": "cancelled",
            "url": format!("https://github.com/acme/orbit/actions/runs/{CODEQL_RUN}/job/{job_id}"),
            "failed_steps": [],
        }],
    })
}

fn four_cancelled_jobs() -> Vec<Value> {
    CODEQL_JOBS
        .iter()
        .enumerate()
        .map(|(index, job_id)| cancelled_zero_step(*job_id, &format!("Analyze ({index})")))
        .collect()
}

#[test]
fn cancelled_zero_step_jobs_file_nothing_and_stay_explicitly_inconclusive() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut evidence = snapshot(Vec::new());
    evidence["inconclusive"] = json!(four_cancelled_jobs());
    evidence["summary"] = json!({"inconclusive": 4, "retryable_errors": 0});

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["outcome"], json!("no_current_failure"));
    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(output["deferred"], json!([]));
    assert_eq!(
        output["inconclusive"]
            .as_array()
            .expect("inconclusive")
            .len(),
        4
    );
    assert_eq!(output["audit"]["inconclusive"], json!(4));
    assert!(
        output["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("inconclusive")),
        "detail must name the cancelled jobs: {output}"
    );
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

#[test]
fn leaked_cancelled_log_404s_are_not_retryable_and_still_file_nothing() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let jobs = four_cancelled_jobs();
    let mut evidence = snapshot(jobs.clone());
    evidence["retryable_errors"] = json!(
        jobs.iter()
            .flat_map(|job| {
                let job_id = job["job_id"].clone();
                [
                    json!({
                        "stage": "investigation",
                        "operation": "run_logs",
                        "run_id": CODEQL_RUN,
                        "job_id": job_id,
                        "retryable": true,
                        "message": "Not Found (HTTP 404)",
                    }),
                    json!({
                        "stage": "investigation",
                        "operation": "run_logs_all",
                        "run_id": CODEQL_RUN,
                        "job_id": job_id,
                        "retryable": true,
                        "message": "Not Found (HTTP 404)",
                    }),
                ]
            })
            .collect::<Vec<_>>()
    );

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["outcome"], json!("no_current_failure"));
    assert_eq!(output["filed_count"], json!(0));
    assert_eq!(
        output["inconclusive"]
            .as_array()
            .expect("inconclusive")
            .len(),
        4
    );
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

#[test]
fn mixed_snapshot_files_the_failed_step_and_lists_the_cancelled_job() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let complete = failure(
        20,
        "ci",
        "build",
        "cargo test",
        "ci\tbuild\t##[error]assertion failed\n",
        CHECKOUT,
    );
    let evidence = snapshot(vec![
        complete,
        cancelled_zero_step(CODEQL_JOBS[0], "Analyze"),
    ]);

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["outcome"], json!("current_failures"));
    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(
        output["inconclusive"]
            .as_array()
            .expect("inconclusive")
            .len(),
        1
    );
    assert_eq!(output["inconclusive"][0]["job_id"], json!(CODEQL_JOBS[0]));
    assert_eq!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .len(),
        1
    );
}

#[test]
fn genuine_failure_log_404_stays_retryable() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut missing_log = failure(10, "ci", "build", "Run CI guardrails", "", CHECKOUT);
    missing_log["log_excerpt"] = json!("");
    let mut evidence = snapshot(vec![missing_log]);
    evidence["retryable_errors"] = json!([{
        "stage": "investigation",
        "operation": "run_logs",
        "run_id": 10,
        "job_id": 910,
        "retryable": true,
        "message": "Not Found (HTTP 404)",
    }]);

    let error = file_error(&runtime, json!({"ci_evidence": evidence}));
    assert!(error.contains("retryable_error"));
    assert!(error.contains("Not Found (HTTP 404)"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}
