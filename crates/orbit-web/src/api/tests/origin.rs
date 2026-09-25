use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use orbit_core::{JobRunState, OrbitRuntime};
use serde_json::json;
use tower::ServiceExt;

use super::super::*;
use super::handlers::{request_cancel, request_tasks};
use super::test_support::{body_json, seed_run};

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
async fn require_localhost_origin_rejects_https_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = seed_run(
        &runtime,
        "jrun-web-cancel-https-origin",
        "web_cancel_https_origin",
        JobRunState::Pending,
    );

    let response = request_cancel(
        runtime.clone(),
        &run.run_id,
        Some("https://localhost"),
        Some("localhost:7878"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let stored = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(stored.state, JobRunState::Pending);
}

#[tokio::test]
async fn require_localhost_origin_matches_loopback_authority_and_effective_http_port() {
    let cases = [
        ("http://localhost:7878", "localhost:7878", "localhost"),
        ("http://127.0.0.1:7878", "127.0.0.1:7878", "127-0-0-1"),
        ("http://[::1]:7878", "[::1]:7878", "ipv6-loopback"),
        ("http://localhost", "localhost", "implicit-http-port"),
        ("http://localhost:80", "localhost", "explicit-http-port"),
    ];

    for (origin, host, label) in cases {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let run = seed_run(
            &runtime,
            &format!("jrun-web-cancel-origin-port-{label}"),
            "web_cancel_origin_port",
            JobRunState::Pending,
        );

        let response = request_cancel(runtime.clone(), &run.run_id, Some(origin), Some(host)).await;

        assert_eq!(response.status(), StatusCode::OK, "{label}");
        let stored = runtime.show_job_run(&run.run_id).expect("show run");
        assert_eq!(stored.state, JobRunState::Cancelled, "{label}");
    }
}

#[tokio::test]
async fn require_localhost_origin_rejects_mismatched_or_malformed_authorities() {
    let cases = [
        (
            Some("http://localhost:3000"),
            Some("localhost:7878"),
            "mismatched port",
        ),
        (
            Some("http://localhost:7878"),
            Some("127.0.0.1:7878"),
            "mismatched host",
        ),
        (Some("http://localhost:7878"), None, "missing host"),
        (
            Some("http://localhost:7878"),
            Some("localhost:not-a-port"),
            "malformed host",
        ),
        (Some("null"), Some("localhost:7878"), "null origin"),
        (
            Some("http://localhost:7878/path"),
            Some("localhost:7878"),
            "malformed origin",
        ),
        (
            Some("https://localhost:7878"),
            Some("localhost:7878"),
            "unsupported scheme",
        ),
        (
            Some("http://example.test:7878"),
            Some("example.test:7878"),
            "non-loopback",
        ),
    ];

    for (index, (origin, host, label)) in cases.into_iter().enumerate() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let run = seed_run(
            &runtime,
            &format!("jrun-web-cancel-rejected-authority-{index}"),
            "web_cancel_rejected_authority",
            JobRunState::Pending,
        );

        let response = request_cancel(runtime.clone(), &run.run_id, origin, host).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
        assert_eq!(
            runtime.show_job_run(&run.run_id).expect("show run").state,
            JobRunState::Pending,
            "{label}"
        );
    }
}

#[tokio::test]
async fn require_localhost_origin_does_not_trust_forwarded_authority_headers() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = seed_run(
        &runtime,
        "jrun-web-cancel-forwarded-authority",
        "web_cancel_forwarded_authority",
        JobRunState::Pending,
    );

    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(
            runtime.clone(),
        )))
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(format!("/runs/{}/cancel", run.run_id))
                .header(header::ORIGIN, "http://localhost:7878")
                .header(header::HOST, "example.test:7878")
                .header("x-forwarded-host", "localhost:7878")
                .header("forwarded", "host=localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Pending
    );
}

#[tokio::test]
async fn require_localhost_origin_preserves_missing_origin_behavior() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = seed_run(
        &runtime,
        "jrun-web-cancel-missing-origin",
        "web_cancel_missing_origin",
        JobRunState::Pending,
    );

    let unsafe_response = request_cancel(runtime.clone(), &run.run_id, None, None).await;
    assert_eq!(unsafe_response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        runtime.show_job_run(&run.run_id).expect("show run").state,
        JobRunState::Pending
    );

    let safe_response = request_tasks(runtime).await;
    assert_eq!(safe_response.status(), StatusCode::OK);
    assert_eq!(
        safe_response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
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

#[tokio::test]
async fn require_localhost_origin_accepts_loopback_host_get_without_origin() {
    let cases = [
        "localhost:7878",
        "127.0.0.1:7878",
        "[::1]:7878",
        "localhost",
        "localhost:80",
        "127.0.0.1",
        "[::1]",
    ];

    for host in cases {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let response = router()
            .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/tasks")
                    .header(header::HOST, host)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK, "{host}");
        assert_eq!(
            response.headers().get("x-content-type-options"),
            Some(&HeaderValue::from_static("nosniff")),
            "{host}"
        );
    }
}

#[tokio::test]
async fn require_localhost_origin_rejects_missing_or_unparsable_host() {
    let cases: [(Option<&str>, &str); 3] = [
        (None, "missing host"),
        (Some("localhost:not-a-port"), "unparsable host"),
        (Some("user@localhost"), "userinfo host"),
    ];

    for (host, label) in cases {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let mut builder = Request::builder().method(Method::GET).uri("/tasks");
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        let response = router()
            .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
            .oneshot(builder.body(Body::empty()).expect("request"))
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{label}");
    }
}

async fn request_healthz(host: Option<&str>, uri: &str) -> axum::response::Response {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let mut builder = Request::builder().method(Method::GET).uri(uri);
    if let Some(host) = host {
        builder = builder.header(header::HOST, host);
    }
    crate::health_router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

#[tokio::test]
async fn healthz_require_localhost_origin_rejects_forged_host_detailed() {
    let response = request_healthz(Some("attacker.example:7878"), "/healthz?detailed=true").await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        response.headers().get("x-content-type-options"),
        Some(&HeaderValue::from_static("nosniff"))
    );
    let body = body_json(response).await;
    assert_eq!(body, json!({"error": "cross-origin requests not allowed"}));
    let serialized = body.to_string();
    assert!(
        !serialized.contains("workspaces_open")
            && !serialized.contains("sqlite_writable")
            && !serialized.contains("log_sink")
            && !serialized.contains("orbit.jsonl")
            && !serialized.contains("/home/")
            && !serialized.contains(".orbit"),
        "refused detailed healthz must not leak workspace names or paths: {serialized}"
    );
}

#[tokio::test]
async fn healthz_require_localhost_origin_accepts_loopback_host() {
    let hosts = [
        "localhost:7878",
        "127.0.0.1:7878",
        "[::1]:7878",
        "localhost",
        "localhost:80",
        "127.0.0.1",
        "[::1]",
    ];

    for host in hosts {
        let liveness = request_healthz(Some(host), "/healthz").await;
        assert_eq!(liveness.status(), StatusCode::OK, "liveness {host}");
        let liveness_body = to_bytes(liveness.into_body(), usize::MAX)
            .await
            .expect("read liveness body");
        assert_eq!(&liveness_body[..], b"ok", "liveness {host}");

        let detailed = request_healthz(Some(host), "/healthz?detailed=true").await;
        assert_ne!(
            detailed.status(),
            StatusCode::FORBIDDEN,
            "detailed {host} must pass the Host gate"
        );
        assert!(
            detailed.status() == StatusCode::OK
                || detailed.status() == StatusCode::SERVICE_UNAVAILABLE,
            "detailed {host} status {}",
            detailed.status()
        );
        let body = body_json(detailed).await;
        assert!(
            body["status"] == json!("ok") || body["status"] == json!("fail"),
            "detailed {host} body: {body}"
        );
        assert!(body["checks"].is_array(), "detailed {host} checks: {body}");
        assert!(
            body.get("error").is_none(),
            "detailed {host} must not be the Host-gate error: {body}"
        );
    }
}

#[tokio::test]
async fn require_localhost_origin_blocks_cross_origin_get_with_attacker_origin() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/tasks")
                .header(header::ORIGIN, "http://localhost.evil.com")
                .header(header::HOST, "localhost.evil.com")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
