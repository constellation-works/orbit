use serde_json::json;

use super::super::collect::collect;
use super::support::{
    FakeQueries, HEAD, OLD, current_ids, failed_job, in_flight_ids, input, run, run_on_branch,
};

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
