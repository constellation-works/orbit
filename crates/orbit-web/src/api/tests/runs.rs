use super::super::router;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use orbit_core::OrbitRuntime;
use std::sync::Arc;
use tower::ServiceExt;

async fn request_dashboard_run_events(runtime: OrbitRuntime, encoded_run_id: &str) -> Response {
    request_dashboard_run_events_query(runtime, encoded_run_id, "").await
}

async fn request_dashboard_run_events_query(
    runtime: OrbitRuntime,
    encoded_run_id: &str,
    query: &str,
) -> Response {
    Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{encoded_run_id}/events{query}"))
                .header(header::HOST, "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

#[tokio::test]
async fn list_run_events_rejects_path_traversal_id() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request_dashboard_run_events(runtime, "..%2F..%2Fetc%2Fpasswd").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
