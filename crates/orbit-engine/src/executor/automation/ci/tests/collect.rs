use serde_json::{Value, json};

use super::super::collect::{collect, collect_at, collect_for_sweep};
use super::super::history::RetryableHistory;
use super::super::query::LogScope;
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

#[test]
fn checkout_evidence_uses_all_scope_and_each_log_read_uses_the_requested_bound() {
    const RUN_ID: u64 = 88_004;
    const JOB_ID: u64 = 88_005;
    const LOG_MAX_BYTES: usize = 128;

    let failed_log = format!("compile failed\n{}", "x".repeat(512));
    let all_log = format!(
        "HEAD is now at {HEAD} workflow checkout\n{}",
        "x".repeat(512)
    );
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![vec![run(
            RUN_ID,
            "CI",
            HEAD,
            "completed",
            Some("failure"),
            "2026-10-07T02:10:00Z",
        )]])
        .with_failed_jobs(
            RUN_ID,
            json!([{
                "job_id": JOB_ID,
                "name": "check",
                "status": "completed",
                "conclusion": "failure",
                "failed_steps": [{"name": "compile", "conclusion": "failure"}],
            }]),
        )
        .with_scoped_log(JOB_ID, LogScope::Failed, &failed_log)
        .with_scoped_log(JOB_ID, LogScope::All, &all_log);
    let mut input = input();
    input["log_max_bytes"] = json!(LOG_MAX_BYTES);

    let evidence = collect(&queries, &input, &mut RetryableHistory::default()).expect("collect");

    let failure = &evidence["current_failures"][0];
    assert_eq!(failure["run_id"], json!(RUN_ID));
    assert_eq!(failure["log_total_bytes"], json!(failed_log.len()));
    assert!(
        failure["log_returned_bytes"]
            .as_u64()
            .expect("returned byte count")
            <= LOG_MAX_BYTES as u64,
        "the failed-scope excerpt must honor log_max_bytes"
    );
    assert_eq!(failure["checkout_evidence_scope"], json!("all"));
    assert_eq!(failure["actual_checkout_shas"], json!([HEAD]));
    assert_eq!(
        *queries.log_reads.lock().expect("log reads"),
        [
            (RUN_ID.to_string(), JOB_ID, LogScope::Failed, LOG_MAX_BYTES),
            (RUN_ID.to_string(), JOB_ID, LogScope::All, LOG_MAX_BYTES),
        ],
        "checkout recovery must read Failed then All for the same run and job"
    );
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

    for (sweep, sweep_id) in [(1, "scheduled-1"), (2, "scheduled-2")] {
        let evidence =
            collect_for_sweep(&queries, &input(), &mut history, sweep_id).expect("collect");
        assert_eq!(
            evidence["outcome_hint"],
            json!("retryable_error"),
            "sweep {sweep} still retries"
        );
        assert_eq!(evidence["persistent_retryable_errors"], json!([]));
    }

    let evidence =
        collect_for_sweep(&queries, &input(), &mut history, "scheduled-3").expect("collect");
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

#[test]
fn repeated_collection_for_one_sweep_does_not_advance_persistence() {
    let queries = concurrency_cancelled_pr_runs();
    let mut history = RetryableHistory::default();

    for retry in 1..=3 {
        let evidence =
            collect_for_sweep(&queries, &input(), &mut history, "scheduled-1").expect("collect");
        assert_eq!(
            evidence["outcome_hint"],
            json!("retryable_error"),
            "collection retry {retry} belongs to the same sweep"
        );
        assert_eq!(evidence["persistent_retryable_errors"], json!([]));
    }

    let evidence =
        collect_for_sweep(&queries, &input(), &mut history, "scheduled-2").expect("collect");
    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
    assert_eq!(evidence["persistent_retryable_errors"], json!([]));
}

#[test]
fn unavailable_collection_breaks_a_consecutive_error_streak() {
    let error = json!({
        "run_id": 42,
        "job_id": 24,
        "operation": "job_log_truncated",
        "retryable": true,
    });
    let mut history = RetryableHistory::default();
    assert!(
        history
            .observe(vec![error.clone()], "scheduled-1")
            .persistent
            .is_empty()
    );
    assert!(
        history
            .observe(vec![error.clone()], "scheduled-2")
            .persistent
            .is_empty()
    );

    let evidence = collect_for_sweep(
        &FakeQueries::unauthenticated(),
        &input(),
        &mut history,
        "scheduled-3",
    )
    .expect("collect unavailable capability");
    assert_eq!(evidence["collected"], json!(false));

    let after_gap = history.observe(vec![error], "scheduled-4");
    assert_eq!(after_gap.retryable.len(), 1);
    assert!(after_gap.persistent.is_empty());
}

#[test]
fn query_errors_in_snapshot_notes_are_redacted_without_losing_operation_context() {
    // Synthetic credential; no host credentials or GitHub processes are used.
    let token = format!("ghp_{}", "A".repeat(36));
    let error = format!("query failed: {token}");
    let candidate = || {
        let mut red = run(
            50,
            "CI",
            HEAD,
            "completed",
            Some("failure"),
            "2026-10-07T02:10:00Z",
        );
        red["head_branch"] = json!("candidate");
        FakeQueries::authenticated()
            .with_head("topic", HEAD)
            .with_head("candidate", HEAD)
            .with_runs(vec![vec![red]])
    };
    const PREVIOUS: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SUCCESSOR: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let pending = || {
        let mut red = run(
            200,
            "CI",
            HEAD,
            "completed",
            Some("failure"),
            "2026-10-07T02:10:00Z",
        );
        red["updated_at"] = json!("2026-10-07T02:45:00Z");
        FakeQueries::authenticated()
            .with_head("topic", SUCCESSOR)
            .with_ancestor(HEAD, SUCCESSOR)
            .with_ancestor(PREVIOUS, HEAD)
            .with_runs(vec![vec![
                run(
                    201,
                    "CI",
                    SUCCESSOR,
                    "in_progress",
                    None,
                    "2026-10-07T02:50:00Z",
                ),
                red,
                run(
                    199,
                    "CI",
                    PREVIOUS,
                    "completed",
                    Some("failure"),
                    "2026-10-07T01:10:00Z",
                ),
            ]])
            .with_failed_jobs(
                200,
                json!([{
                    "job_id": 2000, "name": "check", "conclusion": "failure",
                    "failed_steps": [{"name": "compile"}],
                }]),
            )
            .with_failed_jobs(
                199,
                json!([{
                    "job_id": 1990, "name": "check", "conclusion": "failure",
                    "failed_steps": [{"name": "compile"}],
                }]),
            )
    };
    let cases = [
        (
            "closed PR list",
            candidate().with_closed_pull_requests_error(&error),
            "closed pull requests could not be listed",
        ),
        (
            "closed PR head",
            candidate()
                .with_closed_pull_requests_error("listing unavailable")
                .with_closed_pull_request_branch_error("candidate", &error),
            "closed pull requests for candidate branch 'candidate' could not be listed",
        ),
        (
            "open PR head",
            candidate()
                .with_pull_request("CLOSED", 17, "candidate", HEAD)
                .with_open_pull_request_branch_error("candidate", &error),
            "open pull requests for candidate branch 'candidate' could not be listed",
        ),
        (
            "remote head",
            candidate().with_head_error("candidate", &error),
            "branch 'candidate' could not be checked against origin",
        ),
        (
            "successor ancestry",
            pending().with_ancestry_error(HEAD, SUCCESSOR, &error),
            "run 200 could not be compared with in-flight run 201",
        ),
        (
            "previous ancestry",
            pending().with_ancestry_error(PREVIOUS, HEAD, &error),
            "run 200 could not be compared with its previous completed run 199",
        ),
        (
            "run view",
            pending().with_run_view_error(200, &error),
            "run view 200 could not be read to compare run 200",
        ),
        (
            "job log",
            pending().with_run_log_error(2000, &error),
            "failed-step log of job 2000 in run 200 could not be read",
        ),
    ];
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-07T02:55:00Z")
        .expect("fixture time")
        .with_timezone(&chrono::Utc);
    let mut leaked = Vec::new();
    for (case, queries, operation) in cases {
        let evidence = collect_at(
            &queries,
            &input(),
            &mut RetryableHistory::default(),
            "redaction-regression",
            now,
        )
        .expect("collect partial evidence");
        let notes = evidence["truncation"]["notes"].as_array().expect("notes");
        assert!(
            notes
                .iter()
                .filter_map(Value::as_str)
                .any(|note| note.contains(operation) && note.contains("query failed:")),
            "{case}: the note must identify the failed operation and retain its safe error detail"
        );
        if notes
            .iter()
            .filter_map(Value::as_str)
            .any(|note| note.contains(&token))
        {
            leaked.push(case);
        }
    }
    assert!(
        leaked.is_empty(),
        "CI notes exposed credentials for {leaked:?}"
    );
}

fn failed_job(job_id: u64) -> Value {
    json!([{
        "job_id": job_id,
        "name": "check",
        "status": "completed",
        "conclusion": "failure",
        "failed_steps": [{"name": "compile", "conclusion": "failure"}],
    }])
}

/// A log that proves checkout identity without needing a follow-up read.
fn checkout_log() -> String {
    format!("HEAD is now at {HEAD} workflow checkout\nrun output\n")
}

fn budget_errors_for(evidence: &Value, run_id: u64) -> Vec<Value> {
    evidence["retryable_errors"]
        .as_array()
        .expect("retryable errors")
        .iter()
        .filter(|error| {
            error["operation"] == json!("investigation_budget") && error["run_id"] == json!(run_id)
        })
        .cloned()
        .collect()
}

fn budget_of_one() -> Value {
    let mut input = input();
    input["max_investigated_runs"] = json!(1);
    input
}

#[test]
fn over_budget_in_flight_candidate_yields_an_investigation_budget_error() {
    const COMPLETED_RUN: u64 = 101;
    const IN_FLIGHT_RUN: u64 = 102;
    const JOB_ID: u64 = 9_101;
    // The completed failure is newer, so it keeps the single investigation
    // slot and the in-flight mixed-state candidate overflows the budget.
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![vec![
            run(
                COMPLETED_RUN,
                "ci-a",
                HEAD,
                "completed",
                Some("failure"),
                "2026-10-07T02:20:00Z",
            ),
            run(
                IN_FLIGHT_RUN,
                "ci-b",
                HEAD,
                "in_progress",
                None,
                "2026-10-07T02:10:00Z",
            ),
        ]])
        .with_failed_jobs(COMPLETED_RUN, failed_job(JOB_ID))
        .with_log(JOB_ID, &checkout_log());

    let evidence =
        collect(&queries, &budget_of_one(), &mut RetryableHistory::default()).expect("collect");

    assert_eq!(
        sorted_run_ids(&evidence, "current_failures"),
        [COMPLETED_RUN]
    );
    assert!(
        sorted_run_ids(&evidence, "in_flight").contains(&IN_FLIGHT_RUN),
        "the overflowed run is still listed as in-flight"
    );
    // The unexpanded in-flight summary carries no failed jobs, so it cannot
    // survive the actionable-failure filter; the budget error is what marks
    // the gap instead of silence.
    for key in ["current_failures", "branch_failures", "deferred"] {
        assert!(
            !sorted_run_ids(&evidence, key).contains(&IN_FLIGHT_RUN),
            "{key} must not silently absorb the uninvestigated in-flight run"
        );
    }
    let budget = budget_errors_for(&evidence, IN_FLIGHT_RUN);
    assert_eq!(
        budget.len(),
        1,
        "the overflowed in-flight run needs one budget error"
    );
    assert_eq!(budget[0]["stage"], json!("investigation"));
    assert_eq!(budget[0]["retryable"], json!(true));
    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
}

#[test]
fn over_budget_completed_failure_keeps_its_investigation_budget_error() {
    const COMPLETED_RUN: u64 = 101;
    const IN_FLIGHT_RUN: u64 = 102;
    const JOB_ID: u64 = 9_102;
    // The in-flight run is newer here, so the completed failure is the one
    // that overflows the single investigation slot.
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![vec![
            run(
                IN_FLIGHT_RUN,
                "ci-b",
                HEAD,
                "in_progress",
                None,
                "2026-10-07T02:20:00Z",
            ),
            run(
                COMPLETED_RUN,
                "ci-a",
                HEAD,
                "completed",
                Some("failure"),
                "2026-10-07T02:10:00Z",
            ),
        ]])
        .with_failed_jobs(IN_FLIGHT_RUN, failed_job(JOB_ID))
        .with_log(JOB_ID, &checkout_log());

    let evidence =
        collect(&queries, &budget_of_one(), &mut RetryableHistory::default()).expect("collect");

    let budget = budget_errors_for(&evidence, COMPLETED_RUN);
    assert_eq!(
        budget.len(),
        1,
        "the overflowed completed failure keeps its budget error"
    );
    assert_eq!(
        budget[0]["message"],
        json!("current failure was not investigated because max_investigated_runs was exhausted")
    );
    let uninvestigated = evidence["current_failures"]
        .as_array()
        .expect("current failures")
        .iter()
        .find(|failure| failure["run_id"] == json!(COMPLETED_RUN))
        .expect("the overflowed completed failure stays represented");
    assert_eq!(uninvestigated["investigated"], json!(false));
    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
}
