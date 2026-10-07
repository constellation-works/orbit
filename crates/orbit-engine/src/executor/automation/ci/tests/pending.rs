use chrono::{DateTime, Utc};
use serde_json::{Value, json};

use super::super::collect::{collect, collect_at};
use super::super::history::RetryableHistory;
use super::support::{FakeQueries, input, run};

const FAILING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const REPAIRED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LATER: &str = "cccccccccccccccccccccccccccccccccccccccc";
/// When every held run below finished, as GitHub reports a completed run's
/// last update.
const RED_COMPLETED_AT: &str = "2026-10-07T02:45:00Z";

fn at(time: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(time)
        .expect("fixture time")
        .with_timezone(&Utc)
}

/// Collect as of `now`, so the hold window is measured against a fixed clock.
fn collect_as_of(queries: &FakeQueries, input: &Value, now: &str) -> Value {
    collect_at(
        queries,
        input,
        &mut RetryableHistory::default(),
        "direct-collection",
        at(now),
    )
    .expect("collect")
}

fn completed_at(mut run: Value, time: &str) -> Value {
    run["updated_at"] = json!(time);
    run
}

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
            completed_at(
                run(
                    200,
                    "CI",
                    FAILING,
                    "completed",
                    Some("failure"),
                    "2026-10-07T02:10:00Z",
                ),
                RED_COMPLETED_AT,
            ),
        ]])
}

#[test]
fn red_run_is_held_while_a_descendant_push_run_of_its_workflow_is_in_flight() {
    for status in ["in_progress", "queued"] {
        let queries = red_run_with_successor(status).with_ancestor(FAILING, REPAIRED);

        // Ten minutes after the red run completed: inside the default window.
        let evidence = collect_as_of(&queries, &input(), "2026-10-07T02:55:00Z");

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
            pending[0]["pending_since"],
            json!(at(RED_COMPLETED_AT).to_rfc3339())
        );
        assert_eq!(pending[0]["window_minutes"], json!(30));
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

#[test]
fn held_red_run_is_released_once_it_has_waited_past_the_window() {
    let queries = red_run_with_successor("queued").with_ancestor(FAILING, REPAIRED);

    // Thirty-five minutes after the red run completed, its successor is still
    // queued: the failure is filed anyway, naming the run it waited on.
    let evidence = collect_as_of(&queries, &input(), "2026-10-07T03:20:00Z");

    assert!(run_ids(&evidence, "pending_supersession").is_empty());
    assert_eq!(run_ids(&evidence, "current_failures"), [200]);
    let held = &evidence["current_failures"][0]["held_past_window"];
    assert_eq!(held["pending_on"]["run_id"], json!(201));
    assert_eq!(held["pending_on"]["status"], json!("queued"));
    assert_eq!(held["pending_on"]["reported_head_sha"], json!(REPAIRED));
    assert_eq!(
        held["pending_since"],
        json!(at(RED_COMPLETED_AT).to_rfc3339())
    );
    assert_eq!(held["window_minutes"], json!(30));

    // The window is the caller's to widen.
    let mut wide = input();
    wide["pending_supersession_window_minutes"] = json!(60);
    let queries = red_run_with_successor("queued").with_ancestor(FAILING, REPAIRED);
    let evidence = collect_as_of(&queries, &wide, "2026-10-07T03:20:00Z");
    assert_eq!(run_ids(&evidence, "pending_supersession"), [200]);
    assert_eq!(
        evidence["pending_supersession"][0]["window_minutes"],
        json!(60)
    );
}

/// Two consecutive completed red runs and a newer one still queued: agent-main
/// on 2026-10-07, where a `cargo doc` failure reproduced on every completed
/// run and the sweep held each one behind the next queued push for 50 minutes.
fn consecutive_red_runs(second_failed_step: &str) -> FakeQueries {
    let failed_jobs = |job_id: u64, step: &str| {
        json!([{
            "job_id": job_id,
            "name": "Docs",
            "status": "completed",
            "conclusion": "failure",
            "failed_steps": [{"name": step, "conclusion": "failure"}],
        }])
    };
    FakeQueries::authenticated()
        .with_head("topic", LATER)
        .with_runs(vec![vec![
            run(202, "CI", LATER, "queued", None, "2026-10-07T02:50:00Z"),
            completed_at(
                run(
                    201,
                    "CI",
                    REPAIRED,
                    "completed",
                    Some("failure"),
                    "2026-10-07T02:20:00Z",
                ),
                "2026-10-07T02:55:00Z",
            ),
            completed_at(
                run(
                    200,
                    "CI",
                    FAILING,
                    "completed",
                    Some("failure"),
                    "2026-10-07T02:10:00Z",
                ),
                RED_COMPLETED_AT,
            ),
        ]])
        .with_ancestor(FAILING, REPAIRED)
        .with_ancestor(REPAIRED, LATER)
        .with_ancestor(FAILING, LATER)
        .with_failed_jobs(200, failed_jobs(9200, "cargo doc"))
        .with_failed_jobs(201, failed_jobs(9201, second_failed_step))
}

#[test]
fn red_run_that_reproduced_on_the_previous_completed_run_is_not_held() {
    let queries = consecutive_red_runs("cargo doc");

    // One minute after the newest red run completed, well inside the window.
    let evidence = collect_as_of(&queries, &input(), "2026-10-07T02:56:00Z");

    assert!(run_ids(&evidence, "pending_supersession").is_empty());
    assert_eq!(run_ids(&evidence, "current_failures"), [201]);
    assert_eq!(run_ids(&evidence, "stale_or_superseded"), [200]);
    let reproduced = &evidence["current_failures"][0]["reproduced_on"];
    assert_eq!(reproduced["run_id"], json!(200));
    assert_eq!(reproduced["event_reported_head_sha"], json!(FAILING));
    assert_eq!(
        reproduced["shared_failed_steps"],
        json!([{"job": "Docs", "step": "cargo doc"}])
    );
    assert_eq!(reproduced["pending_on"]["run_id"], json!(202));
    assert!(
        evidence["current_failures"][0]
            .get("held_past_window")
            .is_none()
    );
    // Only the newest red run's job is read; the older run is evidence.
    let reads = queries.log_reads.lock().expect("log reads").clone();
    assert!(
        !reads.is_empty() && reads.iter().all(|job| *job == 9201),
        "{reads:?}"
    );
}

#[test]
fn red_run_that_failed_differently_on_the_previous_completed_run_stays_held() {
    let queries = consecutive_red_runs("cargo test");

    let evidence = collect_as_of(&queries, &input(), "2026-10-07T02:56:00Z");

    assert!(run_ids(&evidence, "current_failures").is_empty());
    assert_eq!(run_ids(&evidence, "pending_supersession"), [201]);
    assert_eq!(
        evidence["pending_supersession"][0]["pending_on"]["run_id"],
        json!(202)
    );
    assert!(queries.log_reads.lock().expect("log reads").is_empty());
}
