use serde_json::{Value, json};

use super::super::collect::collect;
use super::super::history::RetryableHistory;
use super::support::{FakeQueries, HEAD, input, run};

fn full_page(count: u64) -> Vec<Value> {
    (1..=count)
        .map(|run_id| {
            run(
                run_id,
                &format!("workflow-{run_id}"),
                HEAD,
                "completed",
                Some("success"),
                "2026-08-30T05:00:00Z",
            )
        })
        .collect()
}

fn cap_notes(evidence: &Value) -> usize {
    evidence["truncation"]["notes"]
        .as_array()
        .expect("truncation notes")
        .iter()
        .filter_map(Value::as_str)
        .filter(|note| note.starts_with("repository-wide workflow runs were listed at the cap"))
        .count()
}

#[test]
fn run_bound_above_the_page_limit_reports_the_applied_limit_and_its_cap() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![full_page(100)]);
    let mut input = input();
    input["max_runs"] = json!(200);

    let evidence = collect(&queries, &input, &mut RetryableHistory::default()).expect("collect");

    assert_eq!(*queries.run_limits.lock().expect("run limits"), [100]);
    assert_eq!(evidence["truncation"]["max_runs"], json!(100));
    assert_eq!(evidence["truncation"]["runs_listed"], json!(100));
    assert_eq!(
        cap_notes(&evidence),
        1,
        "a full page under a clamped bound must still say older runs may be missing"
    );
}

#[test]
fn zero_listing_bounds_are_raised_to_one_so_the_sweep_can_complete() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![full_page(1)]);
    let mut input = input();
    input["max_runs"] = json!(0);
    input["max_pull_requests"] = json!("0");

    let evidence = collect(&queries, &input, &mut RetryableHistory::default()).expect("collect");

    assert_eq!(*queries.run_limits.lock().expect("run limits"), [1]);
    assert_eq!(
        *queries
            .pull_request_limits
            .lock()
            .expect("pull request limits"),
        [1]
    );
    assert_eq!(evidence["truncation"]["max_runs"], json!(1));
    assert_eq!(evidence["truncation"]["max_pull_requests"], json!(1));
    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
}

const PR_BRANCH: &str = "orbit/ORB-14260-6bfc1a2d";
const PR_HEAD: &str = "2222222222222222222222222222222222222222";
const MACOS_RUN: u64 = 37_485_029_423;
const WINDOWS_RUN: u64 = 37_485_029_402;
const MACOS_JOB: u64 = 106_100_000_001;
const WINDOWS_JOB: u64 = 106_100_000_002;

fn pr_run(run_id: u64, workflow: &str, conclusion: &str, created_at: &str) -> Value {
    let mut pr_run = run(
        run_id,
        workflow,
        PR_HEAD,
        "completed",
        Some(conclusion),
        created_at,
    );
    pr_run["event"] = json!("pull_request");
    pr_run["head_branch"] = json!(PR_BRANCH);
    pr_run
}

/// Runs 37485029423 (macOS Platform) and 37485029402 (Windows Compile Check)
/// as on-call saw them on PR #3435: each workflow's earlier run on the branch
/// passed, and the newest was cancelled by its concurrency group mid-step,
/// leaving a step that reads as failed and a log GitHub delivered incomplete.
fn concurrency_cancelled_pr_runs() -> FakeQueries {
    FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head(PR_BRANCH, PR_HEAD)
        .with_pull_request("OPEN", 3435, PR_BRANCH, PR_HEAD)
        .with_runs(vec![vec![
            pr_run(
                37_485_010_001,
                "macOS Platform",
                "success",
                "2026-10-06T14:40:00Z",
            ),
            pr_run(
                37_485_010_002,
                "Windows Compile Check",
                "success",
                "2026-10-06T14:40:00Z",
            ),
            pr_run(
                MACOS_RUN,
                "macOS Platform",
                "cancelled",
                "2026-10-06T15:02:00Z",
            ),
            pr_run(
                WINDOWS_RUN,
                "Windows Compile Check",
                "cancelled",
                "2026-10-06T15:02:00Z",
            ),
        ]])
        .with_failed_jobs(
            MACOS_RUN,
            json!([{
                "job_id": MACOS_JOB,
                "name": "macOS Platform",
                "status": "completed",
                "conclusion": "cancelled",
                "failed_steps": [{"name": "Run platform tests", "conclusion": "failure"}],
            }]),
        )
        .with_failed_jobs(
            WINDOWS_RUN,
            json!([{
                "job_id": WINDOWS_JOB,
                "name": "Windows Compile Check",
                "status": "completed",
                "conclusion": "cancelled",
                "failed_steps": [{"name": "cargo check", "conclusion": "cancelled"}],
            }]),
        )
        .with_incomplete_log(
            MACOS_JOB,
            "Run platform tests\n##[error]The operation was canceled.\n",
        )
        .with_incomplete_log(
            WINDOWS_JOB,
            "cargo check\n##[error]The operation was canceled.\n",
        )
}

fn sorted_run_ids(evidence: &Value, key: &str) -> Vec<u64> {
    let mut ids: Vec<u64> = evidence[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is a list"))
        .iter()
        .filter_map(|entry| entry["run_id"].as_u64())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn error_operations(evidence: &Value, key: &str) -> Vec<String> {
    let mut operations: Vec<String> = evidence[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is a list"))
        .iter()
        .filter_map(|error| error["operation"].as_str().map(ToOwned::to_owned))
        .collect();
    operations.sort_unstable();
    operations.dedup();
    operations
}

#[test]
fn concurrency_cancelled_newest_runs_with_incomplete_logs_are_inconclusive_not_retryable() {
    // Without the annotation the fixture is the incident: every sweep fails
    // on logs a cancelled job was never going to finish.
    let unannotated = collect(
        &concurrency_cancelled_pr_runs(),
        &input(),
        &mut RetryableHistory::default(),
    )
    .expect("collect");
    assert_eq!(unannotated["outcome_hint"], json!("retryable_error"));
    assert!(
        error_operations(&unannotated, "retryable_errors").contains(&"job_log_truncated".into())
    );

    let queries = concurrency_cancelled_pr_runs()
        .with_concurrency_cancellation(
            MACOS_JOB,
            "Canceling since a higher priority waiting request for macOS Platform-pr-3435 exists",
        )
        .with_concurrency_cancellation(
            WINDOWS_JOB,
            "Canceling since a higher priority waiting request for Windows Compile Check-pr-3435 exists",
        );
    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert!(sorted_run_ids(&evidence, "current_failures").is_empty());
    assert!(sorted_run_ids(&evidence, "branch_failures").is_empty());
    // Each is the newest run of its workflow on the branch, so nothing
    // supersedes it: it is inconclusive, not stale.
    assert_eq!(
        sorted_run_ids(&evidence, "inconclusive"),
        [WINDOWS_RUN, MACOS_RUN]
    );
    for finding in evidence["inconclusive"].as_array().expect("inconclusive") {
        assert_eq!(finding["evidence_state"], json!("inconclusive"));
        assert_eq!(
            finding["inconclusive_reason"],
            json!("concurrency_cancelled")
        );
    }
    assert!(
        queries.log_reads.lock().expect("log reads").is_empty(),
        "a concurrency-cancelled job is classified from its view and annotations alone"
    );
}

#[test]
fn a_run_scoped_error_repeated_on_three_sweeps_becomes_a_persistent_note() {
    let queries = concurrency_cancelled_pr_runs();
    let mut history = RetryableHistory::default();

    for sweep in 1..=2 {
        let evidence = collect(&queries, &input(), &mut history).expect("collect");
        assert_eq!(
            evidence["outcome_hint"],
            json!("retryable_error"),
            "sweep {sweep} still retries"
        );
        assert_eq!(evidence["persistent_retryable_errors"], json!([]));
    }

    let evidence = collect(&queries, &input(), &mut history).expect("collect");
    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert!(sorted_run_ids(&evidence, "current_failures").is_empty());
    assert!(sorted_run_ids(&evidence, "branch_failures").is_empty());
    assert_eq!(
        sorted_run_ids(&evidence, "persistent_retryable_errors"),
        [WINDOWS_RUN, MACOS_RUN]
    );
    assert!(
        error_operations(&evidence, "persistent_retryable_errors")
            .contains(&"job_log_truncated".into())
    );
    for error in evidence["persistent_retryable_errors"]
        .as_array()
        .expect("persistent errors")
    {
        assert_eq!(error["consecutive_sweeps"], json!(3));
        assert_eq!(error["retryable"], json!(false));
    }
    assert_eq!(
        sorted_run_ids(&evidence, "persistently_incomplete"),
        [WINDOWS_RUN, MACOS_RUN]
    );
}
