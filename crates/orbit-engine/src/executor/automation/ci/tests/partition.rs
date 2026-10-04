use serde_json::json;

use super::super::collect::collect;
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

    let evidence = collect(&queries, &budget_of_one()).expect("collect");

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

    let evidence = collect(&queries, &budget_of_one()).expect("collect");

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

    let evidence = collect(&queries, &budget_of_one()).expect("collect");

    assert_eq!(run_ids(&evidence, "current_failures"), [10]);
    assert_eq!(
        evidence["current_failures"][0]["failed_jobs"][0]["job_id"],
        json!(1001)
    );
    assert_eq!(run_ids(&evidence, "inconclusive"), [12]);
    assert_eq!(run_ids(&evidence, "stale_or_superseded"), [11]);
    assert_eq!(budget_errors(&evidence), 0);
}
