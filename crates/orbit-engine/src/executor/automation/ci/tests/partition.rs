use serde_json::json;

use super::super::collect::collect;
use super::super::history::RetryableHistory;
use super::support::{FakeQueries, HEAD, input, run};

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

    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

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

fn run_ids(evidence: &serde_json::Value, key: &str) -> Vec<u64> {
    evidence[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is a list"))
        .iter()
        .filter_map(|entry| entry["run_id"].as_u64())
        .collect()
}

fn budget_errors(evidence: &serde_json::Value) -> usize {
    evidence["retryable_errors"]
        .as_array()
        .expect("retryable errors")
        .iter()
        .filter(|error| error["operation"] == json!("investigation_budget"))
        .count()
}

fn budget_of_one() -> serde_json::Value {
    let mut input = input();
    input["max_investigated_runs"] = json!(1);
    input
}

#[test]
fn concurrency_cancelled_runs_are_superseded_without_spending_budget_or_failing_the_sweep() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![vec![
            run(
                32,
                "CodeQL",
                HEAD,
                "completed",
                Some("cancelled"),
                "2026-10-04T12:20:00Z",
            ),
            run(
                31,
                "CodeQL",
                HEAD,
                "completed",
                Some("cancelled"),
                "2026-10-04T12:12:00Z",
            ),
            run(
                30,
                "CodeQL",
                HEAD,
                "completed",
                Some("cancelled"),
                "2026-10-04T12:08:00Z",
            ),
            run(
                40,
                "ci",
                HEAD,
                "completed",
                Some("success"),
                "2026-10-04T12:20:00Z",
            ),
        ]]);

    let evidence =
        collect(&queries, &budget_of_one(), &mut RetryableHistory::default()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
    assert!(run_ids(&evidence, "current_failures").is_empty());
    assert_eq!(evidence["retryable_errors"], json!([]));
    // The newest cancellation has no successor, so it stays inconclusive.
    assert_eq!(run_ids(&evidence, "inconclusive"), [32]);
    let stale = evidence["stale_or_superseded"].as_array().expect("stale");
    let mut superseded: Vec<u64> = stale
        .iter()
        .filter(|entry| entry["reason"] == json!("cancelled_superseded_by_newer_workflow_run"))
        .inspect(|entry| assert_eq!(entry["superseded_by"]["run_id"], json!(32)))
        .filter_map(|entry| entry["run_id"].as_u64())
        .collect();
    superseded.sort_unstable();
    assert_eq!(superseded, [30, 31]);
    assert_eq!(evidence["summary"]["superseded_cancellations"], json!(2));
    assert_eq!(
        evidence["truncation"]["current_failures_investigation_attempted"],
        json!(0)
    );
}

#[test]
fn superseded_cancelled_run_with_a_failed_step_is_still_investigated() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![vec![
            run(
                22,
                "CodeQL",
                HEAD,
                "completed",
                Some("cancelled"),
                "2026-10-04T12:20:00Z",
            ),
            run(
                21,
                "CodeQL",
                HEAD,
                "completed",
                Some("cancelled"),
                "2026-10-04T12:10:00Z",
            ),
        ]])
        .with_failed_jobs(
            21,
            json!([{
                "job_id": 2101,
                "name": "Analyze",
                "conclusion": "cancelled",
                "failed_steps": [{"name": "Perform analysis", "conclusion": "failure"}],
            }]),
        );

    let evidence =
        collect(&queries, &budget_of_one(), &mut RetryableHistory::default()).expect("collect");

    assert_eq!(run_ids(&evidence, "current_failures"), [21]);
    assert_eq!(
        evidence["current_failures"][0]["failed_jobs"][0]["job_id"],
        json!(2101)
    );
    assert_eq!(run_ids(&evidence, "inconclusive"), [22]);
    assert!(run_ids(&evidence, "stale_or_superseded").is_empty());
    // The inconclusive newest run left the only slot to the real failure.
    assert_eq!(
        evidence["truncation"]["current_failures_investigation_attempted"],
        json!(1)
    );
    assert_eq!(budget_errors(&evidence), 0);
}

#[test]
fn newer_cancellations_never_erase_an_older_actionable_failure() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_runs(vec![vec![
            run(
                12,
                "CodeQL",
                HEAD,
                "completed",
                Some("cancelled"),
                "2026-10-04T12:20:00Z",
            ),
            run(
                11,
                "CodeQL",
                HEAD,
                "completed",
                Some("cancelled"),
                "2026-10-04T12:15:00Z",
            ),
            run(
                10,
                "CodeQL",
                HEAD,
                "completed",
                Some("failure"),
                "2026-10-04T12:10:00Z",
            ),
        ]])
        .with_failed_jobs(
            10,
            json!([{
                "job_id": 1001,
                "name": "Analyze",
                "conclusion": "failure",
                "failed_steps": [{"name": "Build", "conclusion": "failure"}],
            }]),
        );

    let evidence =
        collect(&queries, &budget_of_one(), &mut RetryableHistory::default()).expect("collect");

    assert_eq!(run_ids(&evidence, "current_failures"), [10]);
    assert_eq!(
        evidence["current_failures"][0]["failed_jobs"][0]["job_id"],
        json!(1001)
    );
    assert_eq!(run_ids(&evidence, "inconclusive"), [12]);
    assert_eq!(run_ids(&evidence, "stale_or_superseded"), [11]);
    assert_eq!(budget_errors(&evidence), 0);
}

// Script the discovery boundary: an unmerged task PR is retained separately,
// including when the open-PR page omits it, rather than becoming a landing repair.
#[test]
fn collector_separates_task_pr_failures_from_landing_push_failures() {
    let mut pr = run(
        71,
        "ci",
        HEAD,
        "completed",
        Some("failure"),
        "2026-10-04T18:40:00Z",
    );
    pr["event"] = json!("pull_request");
    pr["head_branch"] = json!("orbit/ORB-13887-ddb04571");
    let push = run(
        72,
        "ci",
        HEAD,
        "completed",
        Some("failure"),
        "2026-10-04T18:41:00Z",
    );
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("orbit/ORB-13887-ddb04571", HEAD)
        .with_runs(vec![vec![pr, push]]);
    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");
    assert_eq!(run_ids(&evidence, "current_failures"), [72]);
    assert_eq!(run_ids(&evidence, "branch_failures"), [71]);
}

#[test]
fn landing_checkout_classification_requires_push_identity_or_observed_tip() {
    use super::super::partition::is_landing_failure;
    use super::super::refs::{RefKind, ScannedRef};
    let refs = [ScannedRef {
        kind: RefKind::Integration,
        branch: "topic".into(),
        head_sha: Some(HEAD.into()),
        pr_number: None,
        pr_url: None,
    }];
    let old = "2".repeat(40);
    let foreign = "3".repeat(40);
    for (event, branch, checkout, expected) in [
        ("push", "topic", &old, true),
        ("push", "topic", &foreign, false),
        ("pull_request", "topic", &foreign, false),
        ("pull_request", "orbit/ORB-13887-ddb04571", &foreign, false),
        (
            "pull_request",
            "orbit/ORB-13887-ddb04571",
            &HEAD.to_string(),
            true,
        ),
        (
            "merge_group",
            "gh-readonly-queue/topic/pr-1",
            &HEAD.to_string(),
            true,
        ),
        (
            "merge_group",
            "gh-readonly-queue/topic/pr-1",
            &foreign,
            false,
        ),
    ] {
        let failure = json!({"event": event, "head_branch": branch,
            "event_reported_head_sha": old, "actual_checkout_shas": [checkout]});
        assert_eq!(
            is_landing_failure(&refs, &failure),
            expected,
            "event {event}, branch {branch}"
        );
    }
}

#[test]
fn red_runs_on_closed_pull_request_or_deleted_branches_are_not_current() {
    let closed_head = "a".repeat(40);
    let moved_head = "b".repeat(40);
    let on_branch = |run_id: u64, branch: &str, sha: &str| {
        let mut pr_run = run(
            run_id,
            "ci",
            sha,
            "completed",
            Some("failure"),
            "2026-10-06T15:00:00Z",
        );
        pr_run["event"] = json!("pull_request");
        pr_run["head_branch"] = json!(branch);
        pr_run
    };
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("orbit/ORB-1-closed", &closed_head)
        .with_head("orbit/ORB-3-moved", &moved_head)
        .with_pull_request("CLOSED", 10, "orbit/ORB-1-closed", &closed_head)
        // Closed at an older head: the branch moved on, so its run is live.
        .with_pull_request("CLOSED", 12, "orbit/ORB-3-moved", &closed_head)
        .with_runs(vec![vec![
            on_branch(81, "orbit/ORB-1-closed", &closed_head),
            on_branch(82, "orbit/ORB-2-gone", &closed_head),
            on_branch(83, "orbit/ORB-3-moved", &moved_head),
        ]]);

    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

    assert!(run_ids(&evidence, "current_failures").is_empty());
    assert_eq!(run_ids(&evidence, "branch_failures"), [83]);
    let stale = evidence["stale_or_superseded"].as_array().expect("stale");
    let reason = |run_id: u64| {
        stale
            .iter()
            .find(|entry| entry["run_id"] == json!(run_id))
            .map(|entry| entry["reason"].clone())
    };
    assert_eq!(reason(81), Some(json!("pull_request_closed")));
    assert_eq!(reason(82), Some(json!("ref_no_longer_exists")));
    assert_eq!(reason(83), None);
    assert_eq!(
        evidence["truncation"]["closed_pull_request_refs"],
        json!(["orbit/ORB-1-closed"])
    );
    assert_eq!(
        evidence["truncation"]["retired_refs"],
        json!(["orbit/ORB-2-gone"])
    );
}

#[test]
fn closed_pull_request_omitted_by_full_listing_is_found_by_head() {
    let branch = "old-closed-branch";
    let closed_head = "c".repeat(40);
    let mut queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head(branch, &closed_head);
    for number in 1..=100 {
        queries = queries.with_pull_request(
            "CLOSED",
            number,
            &format!("recent-closed-{number}"),
            &"d".repeat(40),
        );
    }
    let mut stale_run = run(
        91,
        "ci",
        &closed_head,
        "completed",
        Some("failure"),
        "2026-10-06T15:00:00Z",
    );
    stale_run["event"] = json!("pull_request");
    stale_run["head_branch"] = json!(branch);
    let queries = queries
        .with_closed_pull_request_for_branch(branch, 1001, &closed_head)
        .with_runs(vec![vec![stale_run]]);

    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

    assert!(run_ids(&evidence, "current_failures").is_empty());
    let stale = evidence["stale_or_superseded"].as_array().expect("stale");
    let entry = stale
        .iter()
        .find(|entry| entry["run_id"] == json!(91))
        .expect("closed branch is retired");
    assert_eq!(entry["reason"], json!("pull_request_closed"));
    assert_eq!(entry["pr_number"], json!(1001));
    assert_eq!(
        *queries
            .closed_pull_request_branch_queries
            .lock()
            .expect("closed PR branch queries"),
        [branch]
    );
    assert_eq!(
        *queries
            .open_pull_request_branch_queries
            .lock()
            .expect("open PR branch queries"),
        [branch]
    );
}

#[test]
fn failed_branch_specific_closed_pr_lookup_defers_the_failure() {
    let branch = "old-closed-branch";
    let branch_head = "c".repeat(40);
    let mut queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head(branch, &branch_head)
        .with_closed_pull_request_branch_error(branch, "temporary API failure");
    for number in 1..=100 {
        queries = queries.with_pull_request(
            "CLOSED",
            number,
            &format!("recent-closed-{number}"),
            &"d".repeat(40),
        );
    }
    let mut run = run(
        93,
        "ci",
        &branch_head,
        "completed",
        Some("failure"),
        "2026-10-06T15:00:00Z",
    );
    run["event"] = json!("pull_request");
    run["head_branch"] = json!(branch);
    let queries = queries.with_runs(vec![vec![run]]);

    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

    assert!(run_ids(&evidence, "current_failures").is_empty());
    assert!(run_ids(&evidence, "branch_failures").is_empty());
    assert_eq!(
        evidence["retryable_errors"][0]["operation"],
        json!("closed_pull_request_head")
    );
    assert_eq!(
        *queries
            .closed_pull_request_branch_queries
            .lock()
            .expect("closed PR branch queries"),
        [branch]
    );
}

#[test]
fn failed_global_closed_pr_listing_falls_back_to_branch_lookup() {
    let branch = "old-closed-branch";
    let branch_head = "c".repeat(40);
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head(branch, &branch_head)
        .with_closed_pull_requests_error("temporary API failure")
        .with_closed_pull_request_for_branch(branch, 1001, &branch_head);
    let mut stale_run = run(
        94,
        "ci",
        &branch_head,
        "completed",
        Some("failure"),
        "2026-10-06T15:00:00Z",
    );
    stale_run["event"] = json!("pull_request");
    stale_run["head_branch"] = json!(branch);
    let queries = queries.with_runs(vec![vec![stale_run]]);

    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

    assert!(run_ids(&evidence, "current_failures").is_empty());
    let stale = evidence["stale_or_superseded"].as_array().expect("stale");
    let entry = stale
        .iter()
        .find(|entry| entry["run_id"] == json!(94))
        .expect("branch-specific closed PR retires the failure");
    assert_eq!(entry["reason"], json!("pull_request_closed"));
    assert_eq!(entry["pr_number"], json!(1001));
    assert!(
        evidence["retryable_errors"]
            .as_array()
            .expect("errors")
            .is_empty()
    );
    assert_eq!(
        *queries
            .closed_pull_request_branch_queries
            .lock()
            .expect("closed PR branch queries"),
        [branch]
    );
}

#[test]
fn closed_pull_request_does_not_retire_a_branch_with_a_newer_open_pull_request() {
    let branch = "reused-branch";
    let branch_head = "e".repeat(40);
    let mut queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head(branch, &branch_head)
        .with_pull_request("CLOSED", 200, branch, &branch_head)
        .with_open_pull_request_for_branch(branch, 201, &branch_head);
    for number in 1..=10 {
        queries = queries.with_pull_request(
            "OPEN",
            number,
            &format!("unlisted-open-{number}"),
            &"f".repeat(40),
        );
    }
    let mut active_run = run(
        92,
        "ci",
        &branch_head,
        "completed",
        Some("failure"),
        "2026-10-06T15:00:00Z",
    );
    active_run["event"] = json!("pull_request");
    active_run["head_branch"] = json!(branch);
    let queries = queries.with_runs(vec![vec![active_run]]);

    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

    assert_eq!(run_ids(&evidence, "branch_failures"), [92]);
    assert!(
        evidence["stale_or_superseded"]
            .as_array()
            .expect("stale")
            .iter()
            .all(|entry| entry["run_id"] != json!(92))
    );
    assert_eq!(
        *queries
            .open_pull_request_branch_queries
            .lock()
            .expect("open PR branch queries"),
        [branch]
    );
}
