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

fn deferred_ids(evidence: &Value) -> Vec<u64> {
    evidence["deferred"]
        .as_array()
        .expect("deferred")
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
fn green_integration_does_not_suppress_a_red_release_head() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", OLD)
        .with_runs(vec![vec![
            run_on_branch(
                200,
                "ci",
                "topic",
                HEAD,
                "completed",
                Some("success"),
                "2026-08-31T02:00:00Z",
            ),
            run_on_branch(
                100,
                "ci",
                "main",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-31T01:00:00Z",
            ),
        ]])
        .with_run_view("100", json!({"failed_jobs": [failed_job(5, "build")]}))
        .with_log("100", false, "ci\tbuild\tred release assertion failed\n")
        .with_log(
            "100",
            true,
            "ci\tCheckout\tHEAD is now at 2222222222222222222222222222222222222222\n",
        );

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(current_ids(&evidence), [100]);
    assert_eq!(
        evidence["current_failures"][0]["head_branch"],
        json!("main")
    );
    assert_eq!(
        evidence["current_failures"][0]["ref_kind"],
        json!("release")
    );
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
    assert_eq!(evidence["stale_or_superseded"], json!([]));
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
        // The ref is unscanned — no open pull request carries it — but it is
        // still live on origin, so its failure is still somebody's to fix.
        .with_head("side/feature", OLD)
        .with_runs(vec![vec![
            // A newer unsuccessful run on an unscanned ref must not hide the
            // landing-branch failure. Distinct refs stay independently current.
            run_on_branch(
                50,
                "ci",
                "side/feature",
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

/// The jrun-20260905-1932 shape: this workspace lands on `agent-main` while
/// GitHub still reports `main` as the repository default, and most of the red
/// runs in the listing belong to task pull requests that merged hours or days
/// earlier and took their branches with them.
#[test]
fn merged_pull_request_branches_are_retired_while_live_refs_stay_current() {
    let checkout = "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n";
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", HEAD)
        .with_head("main", OLD)
        // The open pull request's branch is live on origin; the merged task
        // branches are gone.
        .with_head("orbit/ORB-11299-open", OLD)
        .with_pull_request(json!({
            "number": 1373,
            "url": "https://github.com/acme/orbit/pull/1373",
            "head_branch": "orbit/ORB-11299-open",
            "reported_head_sha": OLD,
        }))
        .with_runs(vec![vec![
            run_on_branch(
                33986585197,
                "Platform",
                "agent-main",
                HEAD,
                "completed",
                Some("failure"),
                "2026-09-05T19:20:00Z",
            ),
            run_on_branch(
                33986582084,
                "Website",
                "orbit/ORB-11299-open",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-05T19:19:00Z",
            ),
            run_on_branch(
                33900000001,
                "Platform",
                "orbit/ORB-11201-merged",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-04T10:00:00Z",
            ),
            run_on_branch(
                33900000002,
                "Website",
                "orbit/ORB-11150-merged",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-03T10:00:00Z",
            ),
        ]])
        .with_run_view(
            "33986585197",
            json!({"failed_jobs": [failed_job(5, "macOS")]}),
        )
        .with_log("33986585197", false, checkout)
        .with_run_view(
            "33986582084",
            json!({"failed_jobs": [failed_job(6, "build")]}),
        )
        .with_log("33986582084", false, checkout);

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "agent-main", "max_checkout_log_reads": 1}),
    )
    .expect("collect");

    assert_eq!(
        current_ids(&evidence),
        [33_986_585_197, 33_986_582_084],
        "the landing-branch red and the open pull request's red are what is left: {evidence}"
    );
    let retired = evidence["stale_or_superseded"]
        .as_array()
        .expect("stale array")
        .iter()
        .map(|entry| (entry["run_id"].clone(), entry["reason"].clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        retired,
        vec![
            (json!(33_900_000_001_u64), json!("ref_no_longer_exists")),
            (json!(33_900_000_002_u64), json!("ref_no_longer_exists")),
        ],
        "merged task branches are retired, not current: {evidence}"
    );
    assert_eq!(
        evidence["truncation"]["retired_refs"],
        json!(["orbit/ORB-11150-merged", "orbit/ORB-11201-merged"])
    );
}

/// `Website` runs on its own trigger and has no push-workflow counterpart on
/// the landing branch. Nothing else going green may stand in for it.
#[test]
fn a_landing_branch_website_failure_survives_without_a_push_counterpart() {
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", HEAD)
        .with_head("main", OLD)
        .with_runs(vec![vec![
            run_on_branch(
                70,
                "CI",
                "agent-main",
                HEAD,
                "completed",
                Some("success"),
                "2026-09-05T19:30:00Z",
            ),
            run_on_branch(
                71,
                "Website",
                "agent-main",
                HEAD,
                "completed",
                Some("failure"),
                "2026-09-05T19:20:00Z",
            ),
        ]])
        .with_run_view("71", json!({"failed_jobs": [failed_job(9, "deploy")]}))
        .with_log(
            "71",
            false,
            "website\tdeploy\tHEAD is now at 3333333333333333333333333333333333333333\n\
             website\tdeploy\t##[error]sync command not found\n",
        );

    let evidence = collect(
        &queries,
        &json!({"integration_branch": "agent-main", "max_checkout_log_reads": 1}),
    )
    .expect("collect");

    assert_eq!(
        current_ids(&evidence),
        [71],
        "a green CI run is not evidence about the Website workflow: {evidence}"
    );
    assert_eq!(evidence["stale_or_superseded"], json!([]));
}

/// A branch origin cannot be reached about is kept deferred, not current. Failing to look is
/// never evidence that a failure is resolved or current.
#[test]
fn an_unprobeable_branch_keeps_its_failure_deferred() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_branch_head_error("gone/feature", "fatal: unable to access origin")
        .with_runs(vec![vec![run_on_branch(
            60,
            "ci",
            "gone/feature",
            OLD,
            "completed",
            Some("failure"),
            "2026-08-30T05:00:00Z",
        )]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(current_ids(&evidence), Vec::<u64>::new());
    let deferred = evidence["deferred"].as_array().expect("deferred array");
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0]["run_id"], 60);
    assert_eq!(deferred[0]["investigated"], false);
    assert_eq!(evidence["summary"]["deferred_failures"], 1);
    assert_eq!(evidence["summary"]["deferred_failure_run_ids"], json!([60]));
    assert_eq!(evidence["stale_or_superseded"], json!([]));
    assert!(
        evidence["retryable_errors"]
            .as_array()
            .expect("retryable_errors")
            .iter()
            .any(|err| err["operation"] == "remote_branch_head"
                && err["run_id"] == 60
                && err["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("fatal: unable to access origin"))),
        "the unprobeable branch failure carries a remote_branch_head retryable error: {evidence}"
    );
    assert!(
        evidence["truncation"]["notes"]
            .as_array()
            .expect("notes")
            .iter()
            .any(|note| note
                .as_str()
                .is_some_and(|note| note.contains("could not be checked against origin"))),
        "the unprobed branch is reported: {evidence}"
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

#[test]
fn unprobed_branches_past_probe_budget_remain_deferred_without_consuming_investigation_slots() {
    let checkout = "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n";
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            run_on_branch(
                10,
                "integration-ci",
                "topic",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T05:00:00Z",
            ),
            run_on_branch(
                20,
                "feature-ci",
                "feature/unprobed",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-30T04:00:00Z",
            ),
        ]])
        .with_run_view("10", json!({"failed_jobs": [failed_job(1, "build")]}))
        .with_log("10", false, checkout);

    let evidence = collect(
        &queries,
        &json!({
            "integration_branch": "topic",
            "max_retired_ref_probes": 0,
            "max_investigated_runs": 1,
            "max_checkout_log_reads": 1,
        }),
    )
    .expect("collect");

    // The integration failure consumes the 1 investigation slot and is current and investigated
    assert_eq!(current_ids(&evidence), [10]);
    assert_eq!(evidence["summary"]["investigated_failures"], 1);
    assert_eq!(
        evidence["summary"]["investigated_failure_run_ids"],
        json!([10])
    );

    // The candidate branch was past the probe budget (0) so it was deferred without probing
    assert_eq!(deferred_ids(&evidence), [20]);
    let deferred = evidence["deferred"].as_array().expect("deferred array");
    assert_eq!(deferred[0]["investigated"], false);
    assert_eq!(deferred[0]["ref_kind"], "other");
    assert_eq!(deferred[0]["head_branch"], "feature/unprobed");

    // It carries a retryable error for retired_ref_budget
    assert!(
        evidence["retryable_errors"]
            .as_array()
            .expect("retryable errors")
            .iter()
            .any(|err| err["operation"] == "retired_ref_budget"
                && err["run_id"] == 20
                && err["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("max_retired_ref_probes"))),
        "unprobed candidate has retired_ref_budget retryable error: {evidence}"
    );
}

#[test]
fn current_heads_and_verified_open_prs_retain_priority_over_other_refs() {
    let checkout = "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n";
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_head("pr-branch", OLD)
        .with_head("other-live", OLD)
        .with_pull_request(json!({
            "number": 42,
            "url": "https://github.com/acme/orbit/pull/42",
            "head_branch": "pr-branch",
            "reported_head_sha": OLD,
        }))
        .with_runs(vec![vec![
            run_on_branch(
                10,
                "ci-int",
                "topic",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T01:00:00Z",
            ),
            run_on_branch(
                20,
                "ci-rel",
                "main",
                HEAD,
                "completed",
                Some("failure"),
                "2026-08-30T02:00:00Z",
            ),
            run_on_branch(
                30,
                "ci-pr",
                "pr-branch",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-30T03:00:00Z",
            ),
            run_on_branch(
                40,
                "ci-other",
                "other-live",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-30T04:00:00Z",
            ),
        ]])
        .with_run_view("10", json!({"failed_jobs": [failed_job(1, "int")]}))
        .with_log("10", false, checkout)
        .with_run_view("20", json!({"failed_jobs": [failed_job(2, "rel")]}))
        .with_log("20", false, checkout)
        .with_run_view("30", json!({"failed_jobs": [failed_job(3, "pr")]}))
        .with_log("30", false, checkout)
        .with_run_view("40", json!({"failed_jobs": [failed_job(4, "other")]}))
        .with_log("40", false, checkout);

    // Budget of 3 investigated runs:
    // Ranked priority is integration (10) -> release (20) -> PR (30) -> other (40).
    let evidence = collect(
        &queries,
        &json!({
            "integration_branch": "topic",
            "max_investigated_runs": 3,
            "max_checkout_log_reads": 3,
            "investigation_cursor": 0,
        }),
    )
    .expect("collect");

    // All 4 are current failures (other-live was verified on origin)
    assert_eq!(current_ids(&evidence), [10, 20, 30, 40]);
    // But only integration, release, and PR were investigated!
    assert_eq!(
        evidence["summary"]["investigated_failure_run_ids"],
        json!([10, 20, 30])
    );
    assert_eq!(evidence["summary"]["investigated_failures"], 3);

    let current = evidence["current_failures"]
        .as_array()
        .expect("current array");
    let other_run = current.iter().find(|r| r["run_id"] == 40).expect("run 40");
    assert_eq!(other_run["investigated"], false);
}

#[test]
fn bounded_repeated_sweeps_rotate_probes_without_starvation() {
    let mut queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD);

    // 4 candidate branches, none of which exist on origin (all retired)
    let runs = (0..4_u64)
        .map(|index| {
            run_on_branch(
                100 + index,
                &format!("wf-{index}"),
                &format!("candidate-branch-{index}"),
                OLD,
                "completed",
                Some("failure"),
                &format!("2026-08-30T0{index}:00:00Z"),
            )
        })
        .collect::<Vec<_>>();
    queries = queries.with_runs(vec![runs]);

    let retired_for_sweep = |cursor: u64| -> Vec<String> {
        let evidence = collect(
            &queries,
            &json!({
                "integration_branch": "topic",
                "max_retired_ref_probes": 2,
                "investigation_cursor": cursor,
            }),
        )
        .expect("collect");
        evidence["truncation"]["retired_refs"]
            .as_array()
            .expect("retired_refs")
            .iter()
            .map(|r| r.as_str().unwrap().to_string())
            .collect()
    };

    // Candidates in order of appearance in runs (newest run first: candidate-branch-3, 2, 1, 0)
    // Budget is 2. Slot 0 is fixed on candidate-branch-3. Slot 1 rotates through 2, 1, 0.
    let sweep0 = retired_for_sweep(0);
    let sweep1 = retired_for_sweep(1);
    let sweep2 = retired_for_sweep(2);

    let mut all_probed = std::collections::BTreeSet::new();
    all_probed.extend(sweep0);
    all_probed.extend(sweep1);
    all_probed.extend(sweep2);

    assert_eq!(
        all_probed,
        [
            "candidate-branch-0".to_string(),
            "candidate-branch-1".to_string(),
            "candidate-branch-2".to_string(),
            "candidate-branch-3".to_string(),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    );
}

#[test]
fn comprehensive_fixture_classifies_merged_prs_unprobed_live_refs_transient_probe_failures_and_pending_successors()
 {
    let checkout = "ci\tCheckout\tHEAD is now at 3333333333333333333333333333333333333333\n";
    let queries = FakeQueries::authenticated()
        .with_head("agent-main", HEAD)
        .with_head("main", OLD)
        // open PR branch
        .with_head("orbit/ORB-100-open", OLD)
        .with_pull_request(json!({
            "number": 100,
            "url": "https://github.com/acme/orbit/pull/100",
            "head_branch": "orbit/ORB-100-open",
            "reported_head_sha": OLD,
        }))
        // orbit/ORB-200-merged has no head on origin (returns None) -> retired
        // orbit/ORB-300-transient has probe error -> deferred
        .with_branch_head_error(
            "orbit/ORB-300-transient",
            "network timeout contacting origin",
        )
        // orbit/ORB-400-overflow is past max_retired_ref_probes -> deferred
        .with_runs(vec![vec![
            // 1. Integration failure (agent-main)
            run_on_branch(
                1001,
                "Platform",
                "agent-main",
                HEAD,
                "completed",
                Some("failure"),
                "2026-09-05T20:00:00Z",
            ),
            // 2. Release failure (main)
            run_on_branch(
                1002,
                "Platform",
                "main",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-05T19:50:00Z",
            ),
            // 3. Open PR failure (orbit/ORB-100-open)
            run_on_branch(
                1003,
                "Website",
                "orbit/ORB-100-open",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-05T19:40:00Z",
            ),
            // 4. Pending in-flight successor on integration head (run 1004 queued)
            run_on_branch(
                1004,
                "Deploy",
                "agent-main",
                HEAD,
                "in_progress",
                None,
                "2026-09-05T20:10:00Z",
            ),
            // 5. Merged historical PR (orbit/ORB-200-merged)
            run_on_branch(
                1005,
                "Platform",
                "orbit/ORB-200-merged",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-05T18:00:00Z",
            ),
            // 6. Transient probe failure (orbit/ORB-300-transient)
            run_on_branch(
                1006,
                "Platform",
                "orbit/ORB-300-transient",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-05T17:00:00Z",
            ),
            // 7. Unprobed live ref past budget (orbit/ORB-400-overflow)
            run_on_branch(
                1007,
                "Platform",
                "orbit/ORB-400-overflow",
                OLD,
                "completed",
                Some("failure"),
                "2026-09-05T16:00:00Z",
            ),
        ]])
        .with_run_view("1001", json!({"failed_jobs": [failed_job(1, "macOS")]}))
        .with_log("1001", false, checkout)
        .with_run_view("1002", json!({"failed_jobs": [failed_job(2, "linux")]}))
        .with_log("1002", false, checkout)
        .with_run_view("1003", json!({"failed_jobs": [failed_job(3, "web")]}))
        .with_log("1003", false, checkout);

    // With max_retired_ref_probes: 2, candidate branches are:
    // [orbit/ORB-200-merged, orbit/ORB-300-transient, orbit/ORB-400-overflow]
    // Slot 0 probes ORB-200-merged (Ok(None) -> retired)
    // Slot 1 probes ORB-300-transient (Err -> unverified / deferred)
    // ORB-400-overflow is beyond budget -> unverified / deferred
    let evidence = collect(
        &queries,
        &json!({
            "integration_branch": "agent-main",
            "max_retired_ref_probes": 2,
            "max_investigated_runs": 5,
            "max_checkout_log_reads": 5,
            "investigation_cursor": 0,
        }),
    )
    .expect("collect");

    // Current failures only contain the genuine current failures: integration, release, open PR
    assert_eq!(current_ids(&evidence), [1001, 1002, 1003]);
    assert_eq!(
        evidence["summary"]["investigated_failure_run_ids"],
        json!([1001, 1002, 1003])
    );

    // In-flight successor is captured in in_flight
    assert_eq!(in_flight_ids(&evidence), [1004]);

    // Merged PR is retired (in stale_or_superseded)
    let stale = evidence["stale_or_superseded"].as_array().expect("stale");
    assert!(
        stale
            .iter()
            .any(|entry| entry["run_id"] == 1005 && entry["reason"] == "ref_no_longer_exists"),
        "merged PR is retired: {evidence}"
    );

    // Transient failure and unprobed overflow are deferred, NOT current
    assert_eq!(deferred_ids(&evidence), [1006, 1007]);
    let deferred = evidence["deferred"].as_array().expect("deferred");
    assert_eq!(deferred.len(), 2);
    assert_eq!(deferred[0]["investigated"], false);
    assert_eq!(deferred[1]["investigated"], false);

    // Retryable errors carry the run-scoped discovery errors for deferred runs
    let retryable = evidence["retryable_errors"]
        .as_array()
        .expect("retryable_errors");
    assert!(
        retryable
            .iter()
            .any(|err| err["operation"] == "remote_branch_head" && err["run_id"] == 1006),
        "transient probe error is attached to run 1006: {evidence}"
    );
    assert!(
        retryable
            .iter()
            .any(|err| err["operation"] == "retired_ref_budget" && err["run_id"] == 1007),
        "budget exhaustion error is attached to run 1007: {evidence}"
    );
}
