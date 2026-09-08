//! Sibling tests for `health.rs` — `/healthz` liveness + detailed checks
//! [ORB-10005].

use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use orbit_core::OrbitRuntime;
use serde_json::Value;

use crate::health::{HealthQuery, detailed_response, healthz};
use crate::state::DashboardState;

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

fn single_state() -> DashboardState {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    DashboardState::single(Arc::new(runtime))
}

fn check<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["checks"]
        .as_array()
        .expect("checks array")
        .iter()
        .find(|check| check["name"] == name)
        .unwrap_or_else(|| panic!("check '{name}' missing from {body}"))
}

#[tokio::test]
async fn plain_healthz_is_cheap_liveness_200() {
    let response = healthz(State(single_state()), Query(HealthQuery::default())).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read body");
    assert_eq!(&bytes[..], b"ok");
}

#[tokio::test]
async fn detailed_healthz_reports_per_check_status() {
    let state = single_state();
    let log_dir = tempfile::tempdir().expect("tempdir");
    let log_path = log_dir.path().join("orbit.jsonl");

    let response = detailed_response(&state, Ok(log_path)).await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    assert_eq!(body["status"], "ok");
    assert_eq!(check(&body, "sqlite_writable")["status"], "ok");
    assert_eq!(check(&body, "log_sink")["status"], "ok");
    assert!(
        body["checks"]
            .as_array()
            .expect("checks array")
            .iter()
            .all(|check| check["name"] != "graph_index"),
        "retired graph state is not a readiness subsystem: {body}"
    );
}

#[tokio::test]
async fn detailed_healthz_fails_when_store_db_is_broken() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    // The store re-opens `<global root>/orbit.db` per probe; making that
    // path unopenable fails the write probe. (Garbage in the file is not
    // enough: the runtime's live WAL still serves valid pages.)
    let db_path = runtime.global_root().join("orbit.db");
    std::fs::remove_file(&db_path).expect("remove store db");
    std::fs::create_dir(&db_path).expect("block store db path");
    let state = DashboardState::single(Arc::new(runtime));

    let log_dir = tempfile::tempdir().expect("tempdir");
    let response = detailed_response(&state, Ok(log_dir.path().join("orbit.jsonl"))).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = body_json(response).await;
    assert_eq!(body["status"], "fail");
    assert_eq!(check(&body, "sqlite_writable")["status"], "fail");
}

#[tokio::test]
async fn detailed_healthz_does_not_recreate_a_missing_store_db() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let db_path = runtime.global_root().join("orbit.db");
    std::fs::remove_file(&db_path).expect("remove store db");
    let state = DashboardState::single(Arc::new(runtime));
    let log_dir = tempfile::tempdir().expect("tempdir");

    let response = detailed_response(&state, Ok(log_dir.path().join("orbit.jsonl"))).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        check(&body_json(response).await, "sqlite_writable")["status"],
        "fail"
    );
    assert!(
        !db_path.exists(),
        "readiness must not recreate the database"
    );
}

#[tokio::test]
async fn detailed_healthz_fails_when_log_sink_is_unwritable() {
    let state = single_state();
    // A path whose parent is a regular file can never accept appends.
    let dir = tempfile::tempdir().expect("tempdir");
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, b"file, not dir").expect("write blocker");

    let response = detailed_response(&state, Ok(blocker.join("orbit.jsonl"))).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = body_json(response).await;
    assert_eq!(check(&body, "log_sink")["status"], "fail");
}

#[tokio::test]
async fn detailed_healthz_fails_when_log_path_is_unresolvable() {
    let state = single_state();
    let response = detailed_response(&state, Err("no HOME".to_string())).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body = body_json(response).await;
    assert_eq!(check(&body, "log_sink")["status"], "fail");
    assert_eq!(check(&body, "log_sink")["detail"], "no HOME");
}
