//! Operation-mode explanation and governed grant controls over HTTP
//! [ORB-11332].

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::OrbitRuntime;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;

async fn request(
    runtime: OrbitRuntime,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::ORIGIN, "http://localhost:3000")
        .header(header::HOST, "localhost:3000");
    let body = match body {
        Some(value) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("response")
}

#[allow(clippy::await_holding_lock)]
async fn with_caller_env<'a, T>(
    vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _env = orbit_common::test_env::scoped(vars);
    fut.await
}

#[tokio::test]
async fn explain_returns_the_effective_policy_with_sources_and_control_authorization() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = with_caller_env(
        [(OPERATOR_OVERRIDE_ENV, None)],
        request(runtime, Method::GET, "/operation/explain", None),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["policy"]["preset"]["value"], "supervised");
    assert_eq!(payload["policy"]["preset"]["source"], "built-in");
    assert_eq!(
        payload["policy"]["leaf_ceiling"]["source"],
        "preset:supervised@built-in"
    );
    assert_eq!(payload["authority"]["admission"], "none");
    assert_eq!(
        payload["authority"]["reason"],
        "scoped_authorization_required"
    );
    assert!(payload["controls_authorized"].is_boolean());
    assert!(
        payload["limiting_reasons"]
            .as_array()
            .expect("reasons")
            .iter()
            .any(|reason| reason == "scoped_authorization_required")
    );
}

#[tokio::test]
async fn stop_and_revoke_are_refused_for_an_agent_caller() {
    for path in ["/operation/stop", "/operation/revoke"] {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let response = with_caller_env(
            [
                (OPERATOR_OVERRIDE_ENV, None),
                ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
                ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
            ],
            request(
                runtime,
                Method::POST,
                path,
                Some(json!({ "reason": "test" })),
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        let payload = body_json(response).await;
        assert_eq!(payload["code"], "authorization_denied");
    }
}

#[tokio::test]
async fn an_operator_without_a_grant_gets_a_clear_refusal() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let response = with_caller_env(
        [(OPERATOR_OVERRIDE_ENV, Some("1"))],
        request(runtime, Method::POST, "/operation/stop", Some(json!({}))),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = body_json(response).await;
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("no operation grant")),
        "{payload}"
    );
}
