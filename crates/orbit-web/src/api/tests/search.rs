use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{GlobalSearchKind, GlobalSearchParams, OrbitRuntime, TaskStatus};
use serde_json::Value;
use tower::ServiceExt;

use super::super::*;
use super::test_support::body_json;

async fn request_search(runtime: OrbitRuntime, query: &str) -> (StatusCode, Value) {
    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri(format!("/search?{query}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    (response.status(), body_json(response).await)
}

fn seed_search_task(runtime: &OrbitRuntime) {
    runtime
        .add_task(TaskAddParams {
            title: "Bridge hybrid search parity".to_string(),
            description: "Expose ranked search through the HTTP API.".to_string(),
            status: Some(TaskStatus::Backlog),
            tags: vec!["search".to_string(), "api".to_string()],
            context_files: vec!["file:crates/orbit-web/src/api/search.rs".to_string()],
            ..Default::default()
        })
        .expect("seed task");
}

#[tokio::test]
async fn search_route_matches_unified_lexical_pipeline() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    seed_search_task(&runtime);
    let params = GlobalSearchParams {
        query: Some("Bridge hybrid".to_string()),
        kind: GlobalSearchKind::Task,
        limit: 5,
        tags: vec!["search".to_string(), "api".to_string()],
        ..Default::default()
    };
    let expected = serde_json::to_value(runtime.global_search(params).expect("direct search"))
        .expect("serialize direct search");

    let (status, actual) = request_search(
        runtime,
        "query=Bridge+hybrid&kind=task&limit=5&tag=search&tag=api",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn search_route_matches_hybrid_fallback_and_reports_lexical_mode() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let params = GlobalSearchParams {
        query: Some("no indexed vectors".to_string()),
        hybrid: true,
        kind: GlobalSearchKind::Task,
        limit: 5,
        ..Default::default()
    };
    let expected = serde_json::to_value(runtime.global_search(params).expect("direct search"))
        .expect("serialize direct search");

    let (status, actual) = request_search(
        runtime,
        "query=no+indexed+vectors&kind=task&limit=5&hybrid=true",
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(actual, expected);
    assert_eq!(actual["mode"], "lexical");
}

#[tokio::test]
async fn search_route_rejects_invalid_filters_as_bad_requests() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    let (status, body) = request_search(runtime, "query=x&kind=unknown").await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("invalid search kind"))
    );
}

#[tokio::test]
async fn search_route_forwards_semantic_neighbor_mode() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");

    let (status, body) = request_search(
        runtime,
        "query=mutually+exclusive&semantic=ORB-00001&kind=task",
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|error| error.contains("`query` and `semantic` are mutually exclusive"))
    );
}
