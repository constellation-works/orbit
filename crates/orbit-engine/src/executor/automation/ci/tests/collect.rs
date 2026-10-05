use serde_json::{Value, json};

use super::super::collect::collect;
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

    let evidence = collect(&queries, &input).expect("collect");

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

    let evidence = collect(&queries, &input).expect("collect");

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
