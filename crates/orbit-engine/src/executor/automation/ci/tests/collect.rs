use serde_json::json;

use super::super::collect::collect;
use super::support::{
    FakeQueries, HEAD, OLD, current_ids, failed_job, in_flight_ids, input, run, run_on_branch,
};

#[test]
fn unauthenticated_host_stops_before_any_query() {
    let queries = FakeQueries::unauthenticated("gh is present but holds no usable credentials");
    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["collected"], json!(false));
    assert_eq!(evidence["outcome_hint"], json!("capability_unavailable"));
    assert_eq!(evidence["capability"]["authenticated"], json!(false));
    // Nothing that could be misread as "we looked and found nothing".
    assert!(evidence.get("current_failures").is_none());
    assert!(evidence.get("heads").is_none());
    assert!(
        evidence["capability"]["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("no usable credentials"))
    );
}

#[test]
fn separates_event_sha_current_head_and_actual_checkout_commit() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\t2026-08-30T01:00:00Z assertion failed\n")
        .with_log(
            "10",
            true,
            "ci\tCheckout\t2026-08-30T01:00:00Z HEAD is now at 3333333333333333333333333333333333333333\n",
        );

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(failure["event_reported_head_sha"], json!(HEAD));
    assert_eq!(failure["current_ref_head_sha"], json!(HEAD));
    assert_eq!(
        failure["actual_checkout_shas"],
        json!(["3333333333333333333333333333333333333333"])
    );
    assert_eq!(failure["checkout_evidence_scope"], json!("all"));
    assert_eq!(failure["failed_jobs"][0]["name"], json!("build"));
    assert!(
        failure["log_excerpt"]
            .as_str()
            .is_some_and(|log| log.contains("assertion failed"))
    );
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
}

#[test]
fn failed_step_excerpt_and_diagnostic_are_isolated_from_an_oversized_multi_failure_log() {
    let checkout = "3333333333333333333333333333333333333333";
    let failed_log = "build\tTest\t##[group]Run cargo test --workspace\n\
                      build\tTest\t##[endgroup]\n\
                      build\tTest\terror: assertion failed in focused_test\n\
                      build\tTest\t##[error]Process completed with exit code 101.\n";
    let whole_log = format!(
        "ci\tCheckout\tHEAD is now at {checkout}\n{}\
         build\tAllowed failure\t##[group]Run cargo test --keep-going\n\
         build\tAllowed failure\t##[error]Process completed with exit code 2.\n\
         {failed_log}",
        "runner setup output\n".repeat(200)
    );
    assert!(whole_log.len() > 512);
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{
                "job_id": 5,
                "name": "build",
                "conclusion": "failure",
                "failed_steps": [{"name": "Test", "conclusion": "failure"}],
            }]}),
        )
        .with_log("10", false, failed_log)
        .with_log("10", true, &whole_log);

    let evidence = collect(
        &queries,
        &json!({
            "integration_branch": "topic", "log_max_bytes": 512, "max_checkout_log_reads": 1,
        }),
    )
    .expect("collect");
    let failure = &evidence["current_failures"][0];
    let excerpt = failure["log_excerpt"].as_str().expect("excerpt");
    assert!(excerpt.contains("##[group]Run cargo test --workspace"));
    assert!(excerpt.contains("error: assertion failed in focused_test"));
    assert_eq!(failure["log_scope"], "failed");
    assert_eq!(failure["diagnostic_unit"]["step"], "Test");
    assert!(
        failure["diagnostic_unit"]["text"]
            .as_str()
            .is_some_and(|text| text.contains("focused_test"))
    );
    assert_eq!(failure["actual_checkout_shas"], json!([checkout]));
    assert_eq!(evidence["truncation"]["checkout_log_reads"], 1);
    assert_eq!(evidence["retryable_errors"], json!([]));
}

#[test]
fn failed_all_scope_read_does_not_consume_checkout_budget() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view("10", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log(
            "10",
            false,
            "build\tbuild\t##[group]Run cargo test\n\
             build\tbuild\terror: assertion failed\n\
             build\tbuild\t##[error]Process completed with exit code 101.\n",
        )
        .with_log_error("10", true, "temporary full-log failure");

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["truncation"]["checkout_log_reads"], 0);
    assert_eq!(evidence["retryable_errors"][0]["operation"], "run_logs_all");
    assert_eq!(
        evidence["current_failures"][0]["checkout_evidence_scope"],
        "unavailable"
    );
}

#[test]
fn checkout_identity_is_observed_from_the_middle_of_a_bounded_full_log() {
    let checkout = "3333333333333333333333333333333333333333";
    let full_log = format!(
        "head\n{}\nci\tCheckout\tHEAD is now at {checkout}\n{}\ntail\n",
        "x".repeat(500),
        "y".repeat(500),
    );
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\tassertion failed\n")
        .with_log("10", true, &full_log);

    let evidence = collect(
        &queries,
        &json!({
            "integration_branch": "topic", "log_max_bytes": 64, "max_checkout_log_reads": 1,
        }),
    )
    .expect("collect");
    let failure = &evidence["current_failures"][0];

    assert!(failure["log_truncated"].as_bool().is_some());
    assert_eq!(failure["actual_checkout_shas"], json!([checkout]));
    assert_eq!(failure["checkout_identity"]["state"], json!("observed"));
    assert_eq!(
        failure["checkout_identity"]["provenance"]["scope"],
        json!("all")
    );
    assert_eq!(
        failure["checkout_identity"]["provenance"]["source"],
        json!("runner_log")
    );
}

#[test]
fn contradictory_checkout_steps_are_ambiguous_not_a_confident_identity() {
    let first = "3333333333333333333333333333333333333333";
    let second = "4444444444444444444444444444444444444444";
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\tassertion failed\n")
        .with_log(
            "10",
            true,
            &format!(
                "ci\tCheckout\tHEAD is now at {first}\nci\tCheckout\tHEAD is now at {second}\n"
            ),
        );

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];
    assert_eq!(failure["checkout_identity"]["state"], json!("ambiguous"));
    assert_eq!(failure["actual_checkout_shas"], json!([first, second]));
}

#[test]
fn pull_request_head_and_runner_merge_checkout_remain_separate() {
    let merge = "5555555555555555555555555555555555555555";
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", OLD)
        .with_pull_request(json!({
            "number": 12,
            "url": "https://github.com/acme/orbit/pull/12",
            "head_branch": "feature-x",
            "reported_head_sha": HEAD,
        }))
        .with_runs(vec![vec![run_on_branch(
            10,
            "ci",
            "feature-x",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\tassertion failed\n")
        .with_log(
            "10",
            true,
            &format!("ci\tCheckout\tHEAD is now at {merge}\n"),
        );

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];
    assert_eq!(failure["ref_kind"], json!("pull_request"));
    assert_eq!(failure["event_reported_head_sha"], json!(HEAD));
    assert_eq!(failure["current_ref_head_sha"], json!(HEAD));
    assert_eq!(failure["actual_checkout_shas"], json!([merge]));
    assert_eq!(failure["checkout_identity"]["state"], json!("observed"));
}

#[test]
fn empty_failed_step_log_is_an_explicit_retryable_investigation_error() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        );

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];
    assert_eq!(failure["log_excerpt"], json!(""));
    let errors = evidence["retryable_errors"]
        .as_array()
        .expect("retryable_errors");
    assert!(
        errors.iter().any(|error| {
            error["operation"] == json!("run_logs")
                && error["run_id"] == json!(10)
                && error["retryable"] == json!(true)
                && error["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("no failed-step log text"))
        }),
        "empty failed-step log must be retryable, got {errors:?}"
    );
    assert_eq!(failure["investigated"], json!(false));
    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
}

#[test]
fn discovery_failure_is_bounded_redacted_and_retryable() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_repository_runs_error(
            "token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890 could not list workflow runs",
        );

    let evidence = collect(&queries, &input()).expect("collect retryable evidence");

    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
    assert_eq!(evidence["summary"]["retryable_errors"], json!(1));
    let error = &evidence["retryable_errors"][0];
    assert_eq!(error["stage"], json!("discovery"));
    assert_eq!(error["operation"], json!("run_list"));
    assert_eq!(error["retryable"], json!(true));
    assert!(
        !error["message"]
            .as_str()
            .unwrap_or_default()
            .contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890")
    );
}

#[test]
fn investigation_failure_keeps_the_current_run_visible_and_retryable() {
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run_on_branch(
            33_358_160_088,
            "CI",
            "agent-main",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-31T04:00:00Z",
        )]])
        .with_run_view_error("33358160088", "temporary GitHub job-view failure")
        .with_log_error("33358160088", false, "temporary GitHub log failure")
        .with_log_error("33358160088", true, "temporary GitHub full-log failure");

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "agent-main", "max_checkout_log_reads": 1}),
    )
    .expect("collect retryable evidence");

    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
    assert_eq!(
        evidence["current_failures"][0]["run_id"],
        json!(33_358_160_088_u64)
    );
    assert_eq!(
        evidence["current_failures"][0]["investigated"],
        json!(false)
    );
    assert_eq!(evidence["summary"]["current_failures"], json!(1));
    assert_eq!(evidence["summary"]["investigated_failures"], json!(0));
    // Without verified job metadata, no diagnostic or checkout read is safe.
    assert_eq!(evidence["summary"]["retryable_errors"], 1);
    assert_eq!(evidence["retryable_errors"][0]["operation"], "run_view");
    assert_eq!(evidence["truncation"]["job_log_reads"], 0);
    assert_eq!(evidence["truncation"]["checkout_log_reads"], 0);
}

/// ORB-11248: a matrix workflow with enough jobs pushes the checkout-evidence
/// line/commit display cap without the scan itself missing anything. That
/// alone must not stop the failure from being filed.
#[test]
fn checkout_evidence_display_cap_alone_does_not_block_filing() {
    let sha = "3".repeat(40);
    let mut full_log = format!("ci\tCheckout\t2026-08-30T01:00:00Z HEAD is now at {sha}\n");
    for _ in 0..45 {
        full_log.push_str("ci\tCheckout\t2026-08-30T01:00:00Z Checking out the ref\n");
    }
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\tassertion failed\n")
        .with_log("10", true, &full_log);

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(failure["actual_checkout_shas"], json!([sha]));
    assert_eq!(failure["checkout_identity"]["state"], json!("observed"));
    assert_eq!(failure["checkout_evidence_display_truncated"], json!(true));
    assert_eq!(failure["checkout_evidence_complete"], json!(true));
    assert_eq!(failure["investigated"], json!(true));
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
}

/// A dropped overlong line that could have carried checkout identity leaves
/// the scan genuinely incomplete. A SHA found elsewhere in the same log does
/// not lift that: the scan cannot rule out a later, conflicting identity past
/// whatever it failed to read, so this must stay fail-closed (retryable), not
/// be filed on a partial read.
#[test]
fn a_genuinely_incomplete_scan_stays_retryable_even_when_a_sha_was_found() {
    let sha = "4".repeat(40);
    let overlong = "x".repeat(20_000);
    let full_log = format!(
        "ci\tCheckout\t2026-08-30T01:00:00Z HEAD is now at {sha}\n\
         ci\tCheckout\t2026-08-30T01:00:01Z {overlong}\n"
    );
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\tassertion failed\n")
        .with_log("10", true, &full_log);

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(failure["actual_checkout_shas"], json!([sha]));
    assert_eq!(failure["checkout_evidence_complete"], json!(false));
    assert_eq!(failure["checkout_identity"]["state"], json!("incomplete"));
    assert_eq!(failure["investigated"], json!(false));
    let errors = evidence["retryable_errors"]
        .as_array()
        .expect("retryable_errors");
    assert!(
        errors.iter().any(|error| {
            error["operation"] == json!("checkout_evidence")
                && error["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("identity is incomplete"))
        }),
        "a genuine partial scan must stay retryable even with a SHA found: {errors:?}"
    );
    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
}

/// The other half of the same distinction: when the scan is incomplete *and*
/// no SHA was found anywhere, there is genuinely insufficient evidence and
/// the failure must stay retryable rather than being filed on a guess.
#[test]
fn an_incomplete_scan_with_no_sha_found_stays_retryable() {
    let overlong = "z".repeat(20_000);
    let full_log = format!("ci\tCheckout\t2026-08-30T01:00:00Z {overlong}\n");
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\tassertion failed\n")
        .with_log("10", true, &full_log);

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(failure["actual_checkout_shas"], json!([]));
    assert_eq!(failure["checkout_identity"]["state"], json!("incomplete"));
    assert_eq!(failure["investigated"], json!(false));
    let errors = evidence["retryable_errors"]
        .as_array()
        .expect("retryable_errors");
    assert!(
        errors.iter().any(|error| {
            error["operation"] == json!("checkout_evidence")
                && error["message"]
                    .as_str()
                    .is_some_and(|text| text.contains("identity is incomplete"))
        }),
        "insufficient evidence must stay retryable and auditable: {errors:?}"
    );
    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
}

/// A completely absent checkout step (no markers anywhere in the full log) is
/// a distinct, complete-scan case: it must be reported as "missing" identity,
/// not conflated with an incomplete scan, and still stays retryable since
/// there is no evidence at all to file on.
#[test]
fn a_complete_scan_with_no_checkout_step_reports_missing_identity() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            10,
            "ci",
            HEAD,
            "completed",
            Some("failure"),
            "2026-08-30T01:00:00Z",
        )]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [{"job_id": 5, "name": "build", "conclusion": "failure"}]}),
        )
        .with_log("10", false, "ci\tbuild\tassertion failed\n")
        .with_log("10", true, "ci\tbuild\tno checkout markers here\n");

    let evidence = collect(&queries, &input()).expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(failure["checkout_evidence_complete"], json!(true));
    assert_eq!(failure["checkout_identity"]["state"], json!("missing"));
    assert_eq!(failure["investigated"], json!(false));
    let errors = evidence["retryable_errors"]
        .as_array()
        .expect("retryable_errors");
    assert!(
        errors.iter().any(|error| {
            error["operation"] == json!("checkout_evidence")
                && error["message"] == json!("run logs contained no actual checkout SHA")
        }),
        "{errors:?}"
    );
}

#[test]
fn live_failure_fixture_is_latest_current_and_evidence_complete() {
    const EVENT_SHA: &str = "2a4cb4e4631a856552d901b6b062fa6596475cc0";
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", EVENT_SHA)
        .with_head("main", OLD)
        .with_runs(vec![vec![run_on_branch(
            33_358_160_088,
            "CI",
            "agent-main",
            EVENT_SHA,
            "completed",
            Some("failure"),
            "2026-08-31T04:00:00Z",
        )]])
        .with_run_view(
            "33358160088",
            json!({"failed_jobs": [{
                "job_id": 99_384_177_985_u64,
                "name": "Rust tests",
                "conclusion": "failure",
                "url": "https://github.com/danieljhkim/orbit/actions/runs/33358160088/job/99384177985",
                "failed_steps": [{"name": "Run Rust tests", "conclusion": "failure"}],
            }]}),
        )
        .with_log(
            "33358160088",
            false,
            "CI\tRust tests\tRun Rust tests orbit-cli::routine_root::routine_commands_honor_orbit_root_and_mutate_only_the_selected_root\nCI\tRust tests\tcrates/orbit-cli/tests/routine_root.rs:218 routine command touched isolated HOME at /tmp/.tmpgNchET/empty-home\n",
        )
        .with_log(
            "33358160088",
            true,
            "CI\tCheckout\tHEAD is now at 2a4cb4e4631a856552d901b6b062fa6596475cc0\n",
        );

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "agent-main", "max_checkout_log_reads": 1}),
    )
    .expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(failure["run_id"], json!(33_358_160_088_u64));
    assert_eq!(
        failure["failed_jobs"][0]["job_id"],
        json!(99_384_177_985_u64)
    );
    assert_eq!(failure["event_reported_head_sha"], json!(EVENT_SHA));
    assert_eq!(failure["current_ref_head_sha"], json!(EVENT_SHA));
    assert_eq!(failure["actual_checkout_shas"], json!([EVENT_SHA]));
    assert!(
        failure["log_excerpt"]
            .as_str()
            .is_some_and(|log| log.contains("routine command touched isolated HOME"))
    );
    assert_eq!(failure["investigated"], json!(true));
    assert_eq!(
        evidence["summary"]["latest_run_ids"],
        json!([33_358_160_088_u64])
    );
    assert_eq!(
        evidence["summary"]["current_failure_run_ids"],
        json!([33_358_160_088_u64])
    );
    assert_eq!(
        evidence["summary"]["investigated_failure_run_ids"],
        json!([33_358_160_088_u64])
    );
    assert_eq!(evidence["summary"]["retryable_errors"], json!(0));
}

/// The 2026-09-06 current-main macOS failure logged `git log -1 --format=%H`
/// under `UNKNOWN STEP`. Its command/output pair is still runner evidence, so
/// collection must pass the complete failure to the filing sweep instead of
/// deferring it as missing checkout identity.
#[test]
fn unknown_step_git_log_checkout_is_investigated_for_filing() {
    const CHECKOUT: &str = "9a611e053bb440451cdfb5468749327853b12ff9";
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", CHECKOUT)
        .with_head("main", OLD)
        .with_runs(vec![vec![run_on_branch(
            33_948_877_857,
            "macOS Platform",
            "agent-main",
            CHECKOUT,
            "completed",
            Some("failure"),
            "2026-09-06T06:07:50Z",
        )]])
        .with_run_view(
            "33948877857",
            json!({"failed_jobs": [{
                "job_id": 101_260_058_863_u64,
                "name": "macOS tests",
                "conclusion": "failure",
            }]}),
        )
        .with_log("33948877857", false, "macOS Platform\tmacOS tests\tassertion failed\n")
        .with_log(
            "33948877857",
            true,
            "macOS Platform\tUNKNOWN STEP\t2026-09-06T06:07:50.4897836Z [command]/usr/bin/git log -1 --format=%H\n\
             macOS Platform\tUNKNOWN STEP\t2026-09-06T06:07:50.4927165Z 9a611e053bb440451cdfb5468749327853b12ff9\n",
        );

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "agent-main", "max_checkout_log_reads": 1}),
    )
    .expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(failure["actual_checkout_shas"], json!([CHECKOUT]));
    assert_eq!(failure["checkout_identity"]["state"], json!("observed"));
    assert_eq!(failure["investigated"], json!(true));
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
    assert_eq!(evidence["retryable_errors"], json!([]));
}

#[test]
fn incomplete_mixed_state_evidence_stays_retryable_until_logs_are_available() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![run(
            40,
            "ci",
            HEAD,
            "in_progress",
            None,
            "2026-08-30T04:00:00Z",
        )]])
        .with_run_view("40", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log_error("40", false, "logs are not available until the job finishes");

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(
        current_ids(&evidence),
        vec![40],
        "the already-failed job remains visible with a deferred evidence state: {evidence}"
    );
    assert_eq!(
        evidence["current_failures"][0]["evidence_state"],
        "deferred"
    );
    assert_eq!(evidence["current_failures"][0]["investigated"], false);
    assert_eq!(in_flight_ids(&evidence), [40]);
    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
    let errors = evidence["retryable_errors"]
        .as_array()
        .expect("retryable_errors");
    assert!(
        errors.iter().any(|error| {
            error["operation"] == json!("run_logs")
                && error["retryable"] == json!(true)
                && error["run_id"] == json!(40)
        }),
        "missing in-flight logs must stay retryable: {errors:?}"
    );
}

/// A fixed prefix would investigate the same runs every hour and never reach
/// anything below the cap. The ranked prefix keeps its slots; the last one
/// rotates.
#[test]
fn the_investigation_budget_rotates_through_overflow_candidates() {
    let checkout = "ci\tbuild\tHEAD is now at 3333333333333333333333333333333333333333\n";
    let mut queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD);
    let runs = (0..5_u64)
        .map(|index| {
            run(
                10 + index,
                &format!("workflow-{index}"),
                HEAD,
                "completed",
                Some("failure"),
                &format!("2026-08-30T0{index}:00:00Z"),
            )
        })
        .collect::<Vec<_>>();
    for index in 0..5_u64 {
        queries = queries
            .with_run_view(
                &(10 + index).to_string(),
                json!({"failed_jobs": [failed_job(5, "build")]}),
            )
            .with_log(&(10 + index).to_string(), false, checkout);
    }
    let queries = queries.with_runs(vec![runs]);

    let investigated = |cursor: u64| {
        collect(
            &queries,
            &json!({
                "integration_branch": "topic",
                "max_investigated_runs": 3,
                "max_checkout_log_reads": 1,
                "investigation_cursor": cursor,
            }),
        )
        .expect("collect")["summary"]["investigated_failure_run_ids"]
            .clone()
    };

    // Newest first inside the integration tier: 14, 13, 12, 11, 10.
    assert_eq!(investigated(0), json!([14, 13, 12]));
    assert_eq!(investigated(1), json!([14, 13, 11]));
    assert_eq!(investigated(2), json!([14, 13, 10]));
    // One rotation later the cycle repeats, so nothing is starved and the two
    // highest-ranked candidates never lose their slots.
    assert_eq!(investigated(3), json!([14, 13, 12]));
}
