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
