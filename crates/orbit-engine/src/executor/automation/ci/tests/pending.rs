use serde_json::{Value, json};

use super::super::collect::collect;
use super::super::history::RetryableHistory;
use super::support::{FakeQueries, input, run};

const FAILING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REPAIRED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LATER: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn run_ids(evidence: &Value, key: &str) -> Vec<u64> {
    evidence[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is a list"))
        .iter()
        .filter_map(|entry| entry["run_id"].as_u64())
        .collect()
}

fn red_run_with_successor(successor_status: &str) -> FakeQueries {
    FakeQueries::authenticated()
        .with_head("topic", REPAIRED)
        .with_runs(vec![vec![
            run(
                201,
                "CI",
                REPAIRED,
                successor_status,
                None,
                "2026-10-07T02:35:00Z",
            ),
            run(
                200,
                "CI",
                FAILING,
                "completed",
                Some("failure"),
                "2026-10-07T02:10:00Z",
            ),
        ]])
}

#[test]
fn red_run_is_held_while_a_descendant_push_run_of_its_workflow_is_in_flight() {
    for status in ["in_progress", "queued"] {
        let queries = red_run_with_successor(status).with_ancestor(FAILING, REPAIRED);

        let evidence =
            collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

        assert!(
            run_ids(&evidence, "current_failures").is_empty(),
            "{status}"
        );
        assert_eq!(evidence["outcome_hint"], json!("no_current_failure"));
        let pending = &evidence["pending_supersession"];
        assert_eq!(run_ids(&evidence, "pending_supersession"), [200]);
        assert_eq!(
            pending[0]["reason"],
            json!("newer_descendant_run_in_flight")
        );
        assert_eq!(pending[0]["pending_on"]["run_id"], json!(201));
        assert_eq!(pending[0]["pending_on"]["status"], json!(status));
        assert_eq!(
            pending[0]["pending_on"]["reported_head_sha"],
            json!(REPAIRED)
        );
        assert_eq!(pending[0]["event_reported_head_sha"], json!(FAILING));
        assert_eq!(
            evidence["summary"]["pending_supersession_run_ids"],
            json!([200])
        );
        // The held run spent no log read: there is nothing to file from it yet.
        assert!(queries.log_reads.lock().expect("log reads").is_empty());
    }
}

#[test]
fn red_run_stays_current_when_the_in_flight_run_is_not_a_proven_descendant() {
    // Neither a rewritten branch nor an unanswerable ancestry query is proof
    // that the newer run carries a repair.
    for queries in [
        red_run_with_successor("in_progress"),
        red_run_with_successor("in_progress").with_unknown_commit(REPAIRED),
    ] {
        let evidence =
            collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

        assert_eq!(run_ids(&evidence, "current_failures"), [200]);
        assert!(run_ids(&evidence, "pending_supersession").is_empty());
    }

    // A rerun of the failing commit itself is no newer code.
    let queries = FakeQueries::authenticated()
        .with_head("topic", FAILING)
        .with_runs(vec![vec![
            run(
                201,
                "CI",
                FAILING,
                "in_progress",
                None,
                "2026-10-07T02:35:00Z",
            ),
            run(
                200,
                "CI",
                FAILING,
                "completed",
                Some("failure"),
                "2026-10-07T02:10:00Z",
            ),
        ]])
        .with_ancestor(FAILING, FAILING);
    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");
    assert_eq!(run_ids(&evidence, "current_failures"), [200]);
}

#[test]
fn newest_completed_descendant_failure_is_current_and_older_red_runs_stay_superseded() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", LATER)
        .with_runs(vec![vec![
            run(
                202,
                "CI",
                LATER,
                "completed",
                Some("failure"),
                "2026-10-07T02:50:00Z",
            ),
            run(
                200,
                "CI",
                FAILING,
                "completed",
                Some("failure"),
                "2026-10-07T02:10:00Z",
            ),
        ]])
        .with_ancestor(FAILING, LATER);

    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");

    assert_eq!(run_ids(&evidence, "current_failures"), [202]);
    assert!(run_ids(&evidence, "pending_supersession").is_empty());
    assert_eq!(run_ids(&evidence, "stale_or_superseded"), [200]);

    // Once the descendant run goes green, the older failure is suppressed.
    let queries = FakeQueries::authenticated()
        .with_head("topic", REPAIRED)
        .with_runs(vec![vec![
            run(
                201,
                "CI",
                REPAIRED,
                "completed",
                Some("success"),
                "2026-10-07T02:35:00Z",
            ),
            run(
                200,
                "CI",
                FAILING,
                "completed",
                Some("failure"),
                "2026-10-07T02:10:00Z",
            ),
        ]])
        .with_ancestor(FAILING, REPAIRED);
    let evidence = collect(&queries, &input(), &mut RetryableHistory::default()).expect("collect");
    assert!(run_ids(&evidence, "current_failures").is_empty());
    assert!(run_ids(&evidence, "pending_supersession").is_empty());
    assert_eq!(
        evidence["stale_or_superseded"][0]["superseded_by"]["run_id"],
        json!(201)
    );
}
