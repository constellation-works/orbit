use serde_json::json;

use super::super::collect::collect;
use super::support::{
    FakeQueries, HEAD, OLD, current_ids, deferred_ids, failed_job, in_flight_ids, input, run,
    run_on_branch,
};

#[test]
fn loads_remote_branch_heads_once_for_landing_and_retired_refs() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_head("live-feature", OLD)
        .with_runs(vec![vec![
            run_on_branch(
                60,
                "ci",
                "gone-feature",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-30T05:00:00Z",
            ),
            run_on_branch(
                61,
                "ci",
                "live-feature",
                OLD,
                "completed",
                Some("failure"),
                "2026-08-30T04:00:00Z",
            ),
        ]]);

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(queries.branch_head_query_count(), 1);
    assert_eq!(
        evidence["truncation"]["retired_refs"],
        json!(["gone-feature"])
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
