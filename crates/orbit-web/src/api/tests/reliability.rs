//! [ORB-10588] `/api/metrics/reliability`.
//!
//! Covers the contract the UI depends on: every rate ships with its `n`, its
//! denominator label, and the window it was computed over; the excluded
//! outcome buckets stay visible so `success + failed` cannot be mistaken for
//! the run total; and an unbounded window is refused rather than served.

//! Test-only allowlist: endpoint tests use unwrap/expect for fixture setup.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use orbit_core::{JobRunState, OrbitRuntime};
use serde_json::Value;
use tower::ServiceExt;

use super::super::router;
use super::test_support::{body_json, seed_run};

const JOB_ID: &str = "reliability_api";

async fn request_reliability(runtime: OrbitRuntime, query: &str) -> Response {
    Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri(format!("/api/metrics/reliability{query}"))
                .header("host", "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

fn seeded_runtime() -> OrbitRuntime {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    // Two settled outcomes plus three runs that must stay out of the
    // denominator, so the payload has something to disclose.
    seed_run(&runtime, "jrun-ok-1", JOB_ID, JobRunState::Success);
    seed_run(&runtime, "jrun-ok-2", JOB_ID, JobRunState::Success);
    seed_run(&runtime, "jrun-bad-1", JOB_ID, JobRunState::Failed);
    seed_run(&runtime, "jrun-cancel", JOB_ID, JobRunState::Cancelled);
    seed_run(&runtime, "jrun-live", JOB_ID, JobRunState::Running);
    runtime
}

#[tokio::test]
async fn reliability_reports_every_rate_with_its_denominator_and_window() {
    let response = request_reliability(seeded_runtime(), "?window=24h").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let window = &body["window"];
    assert_eq!(window["label"], "24h");
    assert!(
        window["since"].is_string(),
        "the window must state its start"
    );
    assert!(window["until"].is_string(), "the window must state its end");

    let job_runs = &body["totals"]["job_runs"];
    assert_eq!(job_runs["counts"]["total"], 5);
    assert_eq!(job_runs["counts"]["succeeded"], 2);
    assert_eq!(job_runs["counts"]["failed"], 1);
    // The failure rate divides by settled runs only.
    assert_eq!(job_runs["failure_rate"]["numerator"], 1);
    assert_eq!(job_runs["failure_rate"]["denominator"], 3);
    assert!(
        job_runs["failure_rate"]["denominator_label"]
            .as_str()
            .is_some_and(|label| !label.is_empty()),
        "a rate must carry a human-readable denominator label"
    );

    for rate in ["per_step_invocation", "per_job_run"] {
        let node = &body["totals"]["recovery"][rate];
        assert!(node["denominator"].is_number(), "{rate} must state its n");
        assert!(
            node["denominator_label"]
                .as_str()
                .is_some_and(|label| !label.is_empty()),
            "{rate} must name what its denominator counts"
        );
    }
}

#[tokio::test]
async fn excluded_outcomes_stay_visible_so_ok_plus_failed_is_not_the_total() {
    let response = request_reliability(seeded_runtime(), "?window=24h").await;
    let body = body_json(response).await;
    let counts = &body["totals"]["job_runs"]["counts"];

    assert_eq!(counts["cancelled"], 1);
    assert_eq!(counts["in_flight"], 1);
    let settled = counts["succeeded"].as_u64().unwrap() + counts["failed"].as_u64().unwrap();
    assert_ne!(
        settled,
        counts["total"].as_u64().unwrap(),
        "the fixture must exercise the gap between settled runs and all runs"
    );
}

#[tokio::test]
async fn a_thin_denominator_is_flagged_rather_than_rendered_as_confident() {
    let response = request_reliability(seeded_runtime(), "?window=24h").await;
    let body = body_json(response).await;

    // Three settled runs is well under the confidence threshold; the flag is
    // what lets the frontend withhold the percentage.
    assert_eq!(
        body["totals"]["job_runs"]["failure_rate"]["low_sample"],
        true
    );
}

#[tokio::test]
async fn breakdowns_carry_per_job_and_over_time_rates() {
    let response = request_reliability(seeded_runtime(), "?window=24h").await;
    let body = body_json(response).await;
    let workspace = &body["workspaces"][0];

    assert!(workspace["workspace_id"].is_string());
    let by_job = workspace["job_runs"]["by_job"]
        .as_array()
        .expect("by_job array");
    let row = by_job
        .iter()
        .find(|row| row["job_id"] == JOB_ID)
        .expect("seeded job row");
    assert_eq!(row["counts"]["total"], 5);
    assert_eq!(row["failure_rate"]["denominator"], 3);

    let over_time = workspace["job_runs"]["over_time"]
        .as_array()
        .expect("over_time array");
    assert!(
        !over_time.is_empty(),
        "the series must be materialized, empty buckets included"
    );
    assert!(
        over_time
            .iter()
            .all(|bucket| bucket["bucket_start"].is_string()
                && bucket["failure_rate"]["denominator"].is_number())
    );

    // The raw state values behind the classification travel with the payload.
    let observed = &workspace["job_runs"]["observed_states"];
    assert_eq!(observed["success"], 2);
    assert_eq!(observed["failed"], 1);
    assert_eq!(observed["cancelled"], 1);
}

#[tokio::test]
async fn an_unbounded_window_is_refused_rather_than_served_without_a_range() {
    let response = request_reliability(seeded_runtime(), "?window=all").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("explicit time range")),
        "the refusal must say why: {body}"
    );
}

#[tokio::test]
async fn an_unknown_window_is_a_400_not_a_silent_default() {
    let response = request_reliability(seeded_runtime(), "?window=nonsense").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn reliability_payload_is_explicitly_fleet_wide() {
    let response = request_reliability(seeded_runtime(), "?window=7d").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(
        body["scope"].as_str(),
        Some("fleet"),
        "reliability must declare fleet-wide scope so the UI can label it"
    );
}

#[tokio::test]
async fn reliability_7d_window_is_a_half_open_week() {
    let response = request_reliability(seeded_runtime(), "?window=7d").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["window"]["label"], "7d");
    let since =
        chrono::DateTime::parse_from_rfc3339(body["window"]["since"].as_str().expect("since"))
            .expect("parse since");
    let until =
        chrono::DateTime::parse_from_rfc3339(body["window"]["until"].as_str().expect("until"))
            .expect("parse until");
    let span = until.signed_duration_since(since);
    assert!(
        span >= chrono::Duration::days(7) - chrono::Duration::seconds(2)
            && span <= chrono::Duration::days(7) + chrono::Duration::seconds(2),
        "7d reliability span must be ~7 days, got {span}"
    );
}

#[tokio::test]
async fn omitting_the_window_still_yields_an_explicit_one() {
    let response = request_reliability(seeded_runtime(), "").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = body_json(response).await;
    assert!(
        body["window"]["label"]
            .as_str()
            .is_some_and(|l| !l.is_empty()),
        "the default must still be named in the payload"
    );
    assert!(body["window"]["since"].is_string());
}
