//! Cancelled jobs with no failed steps are inconclusive, not log failures.

use serde_json::{Value, json};

use super::super::collect::collect;
use super::support::{FakeQueries, failed_job, run_on_branch};

const HEAD: &str = "1111111111111111111111111111111111111111";
const CODEQL_RUN: u64 = 34_168_609_753;
const CODEQL_JOBS: [u64; 4] = [
    101_884_485_004,
    101_884_485_211,
    101_884_485_221,
    101_884_485_257,
];

fn input() -> Value {
    json!({"integration_branch": "agent-main", "max_checkout_log_reads": 3})
}

fn current_ids(evidence: &Value) -> Vec<u64> {
    evidence["current_failures"]
        .as_array()
        .expect("current failures")
        .iter()
        .filter_map(|run| run["run_id"].as_u64())
        .collect()
}

fn in_flight_ids(evidence: &Value) -> Vec<u64> {
    evidence["in_flight"]
        .as_array()
        .expect("in flight")
        .iter()
        .filter_map(|run| run["run_id"].as_u64())
        .collect()
}

fn inconclusive_job_ids(evidence: &Value) -> Vec<u64> {
    evidence["inconclusive"]
        .as_array()
        .expect("inconclusive")
        .iter()
        .filter_map(|run| run["job_id"].as_u64())
        .collect()
}

fn cancelled_job(job_id: u64, name: &str) -> Value {
    json!({
        "job_id": job_id,
        "name": name,
        "conclusion": "cancelled",
        "url": format!("https://github.com/acme/orbit/actions/runs/{CODEQL_RUN}/job/{job_id}"),
        "failed_steps": [],
    })
}

fn codeql_run() -> Value {
    run_on_branch(
        CODEQL_RUN,
        "CodeQL",
        "agent-main",
        HEAD,
        "completed",
        Some("cancelled"),
        "2026-09-07T23:00:00Z",
    )
}

fn authenticated_heads() -> FakeQueries {
    FakeQueries::authenticated()
        .with_head("agent-main", HEAD)
        .with_head("main", HEAD)
}

fn checkout_log() -> &'static str {
    "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n"
}

/// jrun-20260907-2316: four cancelled CodeQL jobs, empty steps, 404 logs.
#[test]
fn cancelled_zero_step_jobs_are_inconclusive_and_do_not_consume_log_budget() {
    let jobs: Vec<Value> = CODEQL_JOBS
        .iter()
        .enumerate()
        .map(|(index, job_id)| cancelled_job(*job_id, &format!("Analyze ({index})")))
        .collect();
    let queries = authenticated_heads()
        .with_runs(vec![vec![codeql_run()]])
        .with_run_view(
            CODEQL_RUN.to_string().as_str(),
            json!({"failed_jobs": jobs}),
        )
        .with_log_error(
            CODEQL_RUN.to_string().as_str(),
            false,
            "Not Found (HTTP 404)",
        )
        .with_log_error(
            CODEQL_RUN.to_string().as_str(),
            true,
            "Not Found (HTTP 404)",
        );

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert_eq!(current_ids(&evidence), Vec::<u64>::new());
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert_eq!(inconclusive_job_ids(&evidence), CODEQL_JOBS.to_vec());
    assert!(
        evidence["inconclusive"]
            .as_array()
            .expect("inconclusive")
            .iter()
            .all(|entry| {
                entry["evidence_state"] == json!("inconclusive")
                    && entry["inconclusive_reason"] == json!("cancelled_without_failed_steps")
                    && entry["conclusion"] == json!("cancelled")
                    && entry["investigated"] == json!(true)
            })
    );
    assert_eq!(evidence["truncation"]["job_log_reads"], json!(0));
    assert_eq!(evidence["truncation"]["checkout_log_reads"], json!(0));
    assert_eq!(evidence["summary"]["inconclusive"], json!(4));
}

#[test]
fn mixed_cancelled_run_keeps_the_failed_step_and_skips_zero_step_siblings() {
    let queries = authenticated_heads()
        .with_runs(vec![vec![codeql_run()]])
        .with_run_view(
            CODEQL_RUN.to_string().as_str(),
            json!({"failed_jobs": [
                cancelled_job(CODEQL_JOBS[0], "Analyze (javascript)"),
                failed_job(CODEQL_JOBS[1], "Analyze (python)"),
            ]}),
        )
        .with_log(
            CODEQL_RUN.to_string().as_str(),
            false,
            "CodeQL\tAnalyze (python)\t##[error]analysis failed\n",
        )
        .with_log(CODEQL_RUN.to_string().as_str(), true, checkout_log());

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
    assert_eq!(current_ids(&evidence), [CODEQL_RUN]);
    assert_eq!(inconclusive_job_ids(&evidence), [CODEQL_JOBS[0]]);
    assert_eq!(
        evidence["current_failures"][0]["job_id"],
        json!(CODEQL_JOBS[1])
    );
    assert_eq!(evidence["current_failures"][0]["investigated"], json!(true));
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert_eq!(evidence["truncation"]["job_log_reads"], json!(1));
}

#[test]
fn newer_zero_step_cancellation_does_not_supersede_an_older_failure() {
    let queries = authenticated_heads()
        .with_runs(vec![vec![
            codeql_run(),
            run_on_branch(
                20,
                "CodeQL",
                "agent-main",
                HEAD,
                "completed",
                Some("failure"),
                "2026-09-07T22:00:00Z",
            ),
        ]])
        .with_run_view(
            CODEQL_RUN.to_string().as_str(),
            json!({"failed_jobs": [cancelled_job(CODEQL_JOBS[0], "Analyze")]}),
        )
        .with_run_view("20", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log("20", false, "ci\tbuild\tassertion failed\n")
        .with_log("20", true, checkout_log())
        .with_log_error(
            CODEQL_RUN.to_string().as_str(),
            false,
            "Not Found (HTTP 404)",
        );

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(current_ids(&evidence), [20]);
    assert_eq!(inconclusive_job_ids(&evidence), [CODEQL_JOBS[0]]);
    assert_eq!(evidence["stale_or_superseded"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
    assert_eq!(evidence["retryable_errors"], json!([]));
}

#[test]
fn newer_cancelled_run_with_a_failed_step_does_supersede_the_older_failure() {
    let queries = authenticated_heads()
        .with_runs(vec![vec![
            codeql_run(),
            run_on_branch(
                20,
                "CodeQL",
                "agent-main",
                HEAD,
                "completed",
                Some("failure"),
                "2026-09-07T22:00:00Z",
            ),
        ]])
        .with_run_view(
            CODEQL_RUN.to_string().as_str(),
            json!({"failed_jobs": [failed_job(CODEQL_JOBS[1], "Analyze (python)")]}),
        )
        .with_log(
            CODEQL_RUN.to_string().as_str(),
            false,
            "CodeQL\tAnalyze (python)\t##[error]analysis failed\n",
        )
        .with_log(CODEQL_RUN.to_string().as_str(), true, checkout_log())
        .with_run_view("20", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log("20", false, "ci\tbuild\tassertion failed\n")
        .with_log("20", true, checkout_log());

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "agent-main", "max_checkout_log_reads": 3}),
    )
    .expect("collect");

    assert_eq!(current_ids(&evidence), [CODEQL_RUN]);
    assert_eq!(evidence["stale_or_superseded"][0]["run_id"], json!(20));
    assert_eq!(
        evidence["stale_or_superseded"][0]["reason"],
        json!("superseded_by_newer_workflow_run")
    );
    assert_eq!(
        evidence["stale_or_superseded"][0]["superseded_by"]["run_id"],
        json!(CODEQL_RUN)
    );
}

#[test]
fn queued_successor_stays_in_flight_beside_a_zero_step_cancellation() {
    let queries = authenticated_heads()
        .with_runs(vec![vec![
            run_on_branch(
                40,
                "CodeQL",
                "agent-main",
                HEAD,
                "queued",
                None,
                "2026-09-07T23:10:00Z",
            ),
            codeql_run(),
        ]])
        .with_run_view(
            CODEQL_RUN.to_string().as_str(),
            json!({"failed_jobs": [cancelled_job(CODEQL_JOBS[0], "Analyze")]}),
        );

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(in_flight_ids(&evidence), [40]);
    assert_eq!(current_ids(&evidence), Vec::<u64>::new());
    assert_eq!(inconclusive_job_ids(&evidence), [CODEQL_JOBS[0]]);
    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert_eq!(evidence["retryable_errors"], json!([]));
}

#[test]
fn missing_logs_for_a_genuine_failure_stay_retryable() {
    let queries = authenticated_heads()
        .with_runs(vec![vec![run_on_branch(
            10,
            "ci",
            "agent-main",
            HEAD,
            "completed",
            Some("failure"),
            "2026-09-07T22:00:00Z",
        )]])
        .with_run_view("10", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log_error("10", false, "Not Found (HTTP 404)")
        .with_log_error("10", true, "Not Found (HTTP 404)");

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
    assert_eq!(current_ids(&evidence), [10]);
    assert!(
        evidence["retryable_errors"]
            .as_array()
            .expect("errors")
            .iter()
            .any(|error| {
                error["operation"] == json!("run_logs")
                    && error["message"]
                        .as_str()
                        .is_some_and(|text| text.contains("Not Found (HTTP 404)"))
            })
    );
}

#[test]
fn timed_out_jobs_without_steps_are_not_treated_as_inconclusive() {
    let queries = authenticated_heads()
        .with_runs(vec![vec![run_on_branch(
            11,
            "ci",
            "agent-main",
            HEAD,
            "completed",
            Some("timed_out"),
            "2026-09-07T22:00:00Z",
        )]])
        .with_run_view(
            "11",
            json!({"failed_jobs": [{
                "job_id": 9,
                "name": "build",
                "conclusion": "timed_out",
                "failed_steps": [],
            }]}),
        )
        .with_log_error("11", false, "Not Found (HTTP 404)");

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
    assert_eq!(current_ids(&evidence), [11]);
    assert_eq!(evidence["inconclusive"], json!([]));
}

#[test]
fn unauthenticated_host_is_still_capability_unavailable() {
    let queries = FakeQueries::unauthenticated("gh holds no usable credentials");
    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("capability_unavailable"));
    assert!(evidence.get("current_failures").is_none());
    assert!(evidence.get("inconclusive").is_none());
}

#[test]
fn cancelled_run_with_no_failed_jobs_is_inconclusive_not_retryable() {
    let queries = authenticated_heads()
        .with_runs(vec![vec![codeql_run()]])
        .with_run_view(CODEQL_RUN.to_string().as_str(), json!({"failed_jobs": []}));

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert_eq!(current_ids(&evidence), Vec::<u64>::new());
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert_eq!(
        evidence["inconclusive"][0]["inconclusive_reason"],
        json!("cancelled_without_failed_steps")
    );
    assert_eq!(evidence["truncation"]["job_log_reads"], json!(0));
}
