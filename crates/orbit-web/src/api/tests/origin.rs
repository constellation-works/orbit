use super::super::router;
use super::handlers::request_cancel;
use super::test_support::{body_json, seed_run};
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use orbit_core::{JobRunState, OrbitRuntime};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

#[tokio::test]
async fn require_localhost_origin_rejects_prefix_match() {
    let cases = [
        ("http://localhost.evil.com", "localhost prefix"),
        ("http://127.0.0.1.evil.com", "127.0.0.1 prefix"),
    ];

    for (index, (origin, label)) in cases.into_iter().enumerate() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let run = seed_run(
            &runtime,
            &format!("jrun-web-cancel-prefix-{index}"),
            "web_cancel_prefix",
            JobRunState::Pending,
        );

        let response = request_cancel(
            runtime.clone(),
            &run.run_id,
            Some(origin),
            Some("localhost:7878"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
        let stored = runtime.show_job_run(&run.run_id).expect("show run");
        assert_eq!(stored.state, JobRunState::Pending, "{label}");
    }
}

#[tokio::test]
async fn require_localhost_origin_rejects_forged_host_get_without_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/tasks")
                .header(header::HOST, "attacker.example:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
    assert_eq!(
        body_json(response).await["error"],
        json!("cross-origin requests not allowed")
    );
}
