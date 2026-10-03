use super::super::router;
use axum::body::Body;
use axum::http::{Method, Request, header};
use axum::response::Response;
use orbit_core::OrbitRuntime;
use std::sync::Arc;
use tower::ServiceExt;

pub(super) async fn request_cancel(
    runtime: OrbitRuntime,
    run_id: &str,
    origin: Option<&str>,
    host: Option<&str>,
) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/runs/{run_id}/cancel"));
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(host) = host {
        builder = builder.header(header::HOST, host);
    }
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}
