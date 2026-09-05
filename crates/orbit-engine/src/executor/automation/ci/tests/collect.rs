use serde_json::{Value, json};

use super::super::collect::collect;
use super::support::{FakeQueries, failed_job, run, run_on_branch};

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

const HEAD: &str = "1111111111111111111111111111111111111111";
const OLD: &str = "2222222222222222222222222222222222222222";

fn input() -> Value {
    json!({"integration_branch": "topic", "max_checkout_log_reads": 1})
}

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
fn latest_non_failing_run_supersedes_older_failures_on_the_same_head() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            // Newer `ci` run on the same branch, at a different SHA: the older
            // failure is stale even though the branch still points at the SHA
            // that failure tested.
            run_on_branch(
                25,
                "ci",
                "topic",
                OLD,
                "completed",
                Some("success"),
                "2026-08-30T02:30:00Z",
            ),
            run_on_branch(
                20,
                "ci",
                "topic",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T02:00:00Z",
            ),
            // A `ci` run on the release head. It decides that head and nothing
            // else, so it neither creates nor clears a failure on `topic`.
            run_on_branch(
                30,
                "ci",
                "main",
                OLD,
                "completed",
                Some("success"),
                "2026-08-30T03:00:00Z",
            ),
            // A completed non-failing conclusion has the same authority as a
            // success for another workflow.
            run(
                40,
                "docs",
                HEAD,
                "completed",
                Some("skipped"),
                "2026-08-30T04:00:00Z",
            ),
            run(
                35,
                "docs",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T03:30:00Z",
            ),
        ]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["current_failures"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    let reasons: Vec<&str> = evidence["stale_or_superseded"]
        .as_array()
        .expect("stale array")
        .iter()
        .filter_map(|entry| entry["reason"].as_str())
        .collect();
    assert_eq!(
        reasons,
        [
            "superseded_by_newer_workflow_run",
            "superseded_by_newer_workflow_run",
        ]
    );
    let superseding_ids: Vec<u64> = evidence["stale_or_superseded"]
        .as_array()
        .expect("stale array")
        .iter()
        .filter_map(|entry| entry["superseded_by"]["run_id"].as_u64())
        .collect();
    assert_eq!(superseding_ids, [25, 40]);
}

#[test]
fn unrelated_pull_request_success_does_not_suppress_an_integration_failure() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_pull_request(json!({
            "number": 12,
            "url": "https://github.com/acme/orbit/pull/12",
            "head_branch": "feature-x",
            "reported_head_sha": OLD,
        }))
        .with_runs(vec![vec![
            // Newest repository-wide `ci` run is an unrelated pull request.
            // That cannot erase the landing-branch failure on `topic`.
            run_on_branch(
                40,
                "ci",
                "feature-x",
                OLD,
                "completed",
                Some("success"),
                "2026-08-30T04:00:00Z",
            ),
            run_on_branch(
                20,
                "ci",
                "topic",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T02:00:00Z",
            ),
        ]])
        .with_run_view("20", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log("20", false, "ci\tbuild\tassertion failed\n")
        .with_log(
            "20",
            true,
            "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n",
        );

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(current_ids(&evidence), [20]);
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
    assert_eq!(evidence["latest_runs"][0]["run_id"], json!(40));
    assert_eq!(evidence["stale_or_superseded"], json!([]));
}

#[test]
fn distinct_workflow_refs_keep_independent_failures() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            // A newer unsuccessful run on an unscanned ref must not hide the
            // landing-branch failure. Distinct refs stay independently current.
            run_on_branch(
                50,
                "ci",
                "abandoned/feature",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-30T05:00:00Z",
            ),
            run_on_branch(
                20,
                "ci",
                "topic",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T02:00:00Z",
            ),
        ]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(
        current_ids(&evidence),
        [20, 50],
        "landing and unrelated-ref failures stay independently current: {evidence}"
    );
    assert_eq!(
        evidence["current_failures"][0]["ref_kind"],
        json!("integration")
    );
    assert_eq!(evidence["current_failures"][1]["ref_kind"], json!("other"));
    assert_eq!(evidence["stale_or_superseded"], json!([]));
    assert_eq!(evidence["in_flight"], json!([]));
}

#[test]
fn queued_or_running_successor_does_not_suppress_an_observed_failure() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            run(40, "ci", HEAD, "in_progress", None, "2026-08-30T04:00:00Z"),
            run(
                30,
                "ci",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T03:00:00Z",
            ),
            run(50, "lint", HEAD, "queued", None, "2026-08-30T05:00:00Z"),
            run(
                45,
                "lint",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T04:30:00Z",
            ),
        ]])
        .with_run_view("30", json!({"failed_jobs": [failed_job(7, "build")]}))
        .with_log("30", false, "ci\tbuild\tassertion failed\n")
        .with_log(
            "30",
            true,
            "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n",
        )
        .with_run_view("45", json!({"failed_jobs": [failed_job(8, "lint")]}))
        .with_log("45", false, "lint\tlint\tlint failed\n")
        .with_log(
            "45",
            true,
            "lint\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n",
        );

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "topic", "max_checkout_log_reads": 2}),
    )
    .expect("collect");

    assert_eq!(current_ids(&evidence), [45, 30]);
    assert_eq!(in_flight_ids(&evidence), [40, 50]);
    assert_eq!(evidence["stale_or_superseded"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
}

#[test]
fn old_dependabot_failure_is_superseded_by_newer_repository_wide_run() {
    // ORB-11146: an eleven-day-old Dependabot run kept being filed because its
    // branch had not advanced. Repository-wide workflow selection suppresses
    // it once any newer CI run exists, regardless of the newer run's ref.
    const DEPENDABOT_SHA: &str = "0f14f3f2ad2c863f902c0add969ff09d10e3f15c";
    const OLD_RUN_ID: u64 = 31_583_558_682;

    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            run_on_branch(
                40_000_000_000,
                "CI",
                "topic",
                HEAD,
                "completed",
                Some("success"),
                "2026-08-31T01:00:00Z",
            ),
            run_on_branch(
                OLD_RUN_ID,
                "CI",
                "dependabot/cargo/old",
                DEPENDABOT_SHA,
                "completed",
                Some("failure"),
                "2026-08-20T01:00:00Z",
            ),
        ]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["current_failures"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert_eq!(
        evidence["stale_or_superseded"][0]["run_id"],
        json!(OLD_RUN_ID)
    );
}

#[test]
fn newer_cross_branch_success_suppresses_open_pull_request_failure() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_pull_request(json!({
            "number": 959,
            "url": "https://github.com/acme/orbit/pull/959",
            "head_branch": "dependabot/cargo/current",
            "reported_head_sha": OLD,
        }))
        .with_runs(vec![vec![
            run_on_branch(
                200,
                "CI",
                "main",
                HEAD,
                "completed",
                Some("success"),
                "2026-08-31T02:00:00Z",
            ),
            run_on_branch(
                100,
                "CI",
                "dependabot/cargo/current",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-31T01:00:00Z",
            ),
        ]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["current_failures"], json!([]));
    assert_eq!(evidence["latest_runs"][0]["run_id"], json!(200));
    assert_eq!(evidence["stale_or_superseded"][0]["run_id"], json!(100));
}

#[test]
fn latest_selection_is_independent_per_workflow_and_breaks_ties_by_run_id() {
    let tied_at = "2026-08-30T05:00:00Z";
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            // Higher run ID wins the CI tie and suppresses its failure.
            run(52, "ci", HEAD, "completed", Some("success"), tied_at),
            run(51, "ci", HEAD, "completed", Some("failure"), tied_at),
            // Selecting CI must not hide lint's independently latest run.
            run(61, "lint", HEAD, "completed", Some("failure"), tied_at),
            run(
                60,
                "lint",
                HEAD,
                "completed",
                Some("success"),
                "2026-08-30T04:00:00Z",
            ),
        ]]);

    let evidence = collect(&queries, &input()).expect("collect");

    let current_ids: Vec<u64> = evidence["current_failures"]
        .as_array()
        .expect("current failures")
        .iter()
        .filter_map(|run| run["run_id"].as_u64())
        .collect();
    assert_eq!(current_ids, [61]);
    assert_eq!(evidence["stale_or_superseded"][0]["run_id"], json!(51));
    assert_eq!(
        evidence["stale_or_superseded"][0]["superseded_by"]["run_id"],
        json!(52)
    );
}

#[test]
fn derives_release_head_from_github_and_reports_every_bound_it_hit() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", OLD)
        .with_runs(vec![vec![
            run(
                51,
                "a",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T05:00:00Z",
            ),
            run(
                52,
                "b",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T05:00:00Z",
            ),
        ]]);

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "topic", "max_investigated_runs": 1}),
    )
    .expect("collect");

    let heads = evidence["heads"].as_array().expect("heads");
    assert_eq!(heads.len(), 2);
    assert_eq!(heads[0]["kind"], json!("integration"));
    assert_eq!(heads[0]["branch"], json!("topic"));
    // The release branch is whatever GitHub reports as the default, never a
    // guess from a naming convention.
    assert_eq!(heads[1]["kind"], json!("release"));
    assert_eq!(heads[1]["branch"], json!("main"));
    assert_eq!(heads[1]["current_head_sha"], json!(OLD));

    let truncation = &evidence["truncation"];
    // The reported bound is the repository-wide cap that was actually applied.
    assert_eq!(truncation["max_runs"], json!(100));
    assert_eq!(truncation["current_failures_discovered"], json!(2));
    assert_eq!(
        truncation["current_failures_investigation_attempted"],
        json!(1)
    );
    assert_eq!(truncation["current_failures_investigated"], json!(0));
    assert!(
        truncation["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .any(|note| note
                .as_str()
                .is_some_and(|note| note.contains("not investigated"))),
        "truncation must be reported explicitly: {truncation}"
    );
    assert_eq!(
        evidence["current_failures"][0]["investigated"],
        json!(false)
    );
    assert_eq!(
        evidence["current_failures"][1]["investigated"],
        json!(false)
    );
}

#[test]
fn integration_and_release_on_the_same_branch_are_scanned_once() {
    let queries = FakeQueries::authenticated().with_head("main", HEAD);

    let evidence = collect(&queries, &json!({"integration_branch": "main"})).expect("collect");

    assert_eq!(evidence["heads"].as_array().expect("heads").len(), 1);
    assert_eq!(evidence["heads"][0]["kind"], json!("integration"));
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
    assert!(
        evidence["summary"]["retryable_errors"]
            .as_u64()
            .is_some_and(|count| count >= 3)
    );
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

/// ORB-11278: a failed job inside an in-progress workflow is current once
/// evidence is complete. A still-running sibling check is not a repair task.
#[test]
fn failed_job_with_running_siblings_is_current_and_pending_sibling_is_not() {
    const EVENT_SHA: &str = "8b0a760bae17b6f0aeee9eab47840684697fa812";
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", EVENT_SHA)
        .with_head("main", OLD)
        .with_runs(vec![vec![run_on_branch(
            33_979_680_684,
            "CI",
            "agent-main",
            EVENT_SHA,
            "in_progress",
            None,
            "2026-09-05T16:00:00Z",
        )]])
        .with_run_view(
            "33979680684",
            json!({"failed_jobs": [{
                "job_id": 11,
                "name": "Linux tests",
                "conclusion": "failure",
                "url": "https://github.com/danieljhkim/orbit/actions/runs/33979680684/job/11",
                "failed_steps": [{"name": "Run tests", "conclusion": "failure"}],
            }]}),
        )
        .with_log(
            "33979680684",
            false,
            "CI\tLinux tests\tRun tests assertion failed in collect.rs\n",
        )
        .with_log(
            "33979680684",
            true,
            "CI\tCheckout\tHEAD is now at 8b0a760bae17b6f0aeee9eab47840684697fa812\n",
        );

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "agent-main", "max_checkout_log_reads": 1}),
    )
    .expect("collect");
    let failure = &evidence["current_failures"][0];

    assert_eq!(current_ids(&evidence), [33_979_680_684_u64]);
    assert_eq!(failure["status"], json!("in_progress"));
    assert_eq!(failure["failed_jobs"].as_array().map(Vec::len), Some(1));
    assert_eq!(failure["failed_jobs"][0]["name"], json!("Linux tests"));
    assert_eq!(failure["investigated"], json!(true));
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
    assert_eq!(in_flight_ids(&evidence), [33_979_680_684_u64]);
    assert_eq!(evidence["retryable_errors"], json!([]));
}

#[test]
fn pending_in_flight_check_alone_is_not_a_repair_task() {
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
        )]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["current_failures"], json!([]));
    assert_eq!(in_flight_ids(&evidence), [40]);
    assert_eq!(evidence["retryable_errors"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
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
        Vec::<u64>::new(),
        "incomplete mixed-state evidence must not become a repair task: {evidence}"
    );
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

#[test]
fn unrelated_pull_request_in_flight_does_not_suppress_an_integration_failure() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_pull_request(json!({
            "number": 12,
            "url": "https://github.com/acme/orbit/pull/12",
            "head_branch": "feature-x",
            "reported_head_sha": OLD,
        }))
        .with_runs(vec![vec![
            run_on_branch(
                40,
                "ci",
                "feature-x",
                OLD,
                "in_progress",
                None,
                "2026-08-30T04:00:00Z",
            ),
            run_on_branch(
                20,
                "ci",
                "topic",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T02:00:00Z",
            ),
        ]])
        .with_run_view("20", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log("20", false, "ci\tbuild\tassertion failed\n")
        .with_log(
            "20",
            true,
            "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n",
        );

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(current_ids(&evidence), [20]);
    assert_eq!(in_flight_ids(&evidence), [40]);
    assert_eq!(evidence["stale_or_superseded"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
}

#[test]
fn same_ref_rerun_success_suppresses_the_resolved_failure() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            run(
                40,
                "ci",
                HEAD,
                "completed",
                Some("success"),
                "2026-08-30T04:00:00Z",
            ),
            run(
                30,
                "ci",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T03:00:00Z",
            ),
        ]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["current_failures"], json!([]));
    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert_eq!(evidence["stale_or_superseded"][0]["run_id"], json!(30));
    assert_eq!(
        evidence["stale_or_superseded"][0]["superseded_by"]["run_id"],
        json!(40)
    );
}
