use super::super::router;
use super::test_support::body_json;
use crate::state::DashboardState;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::OrbitRuntime;
use serde_json::Value;
use std::sync::Arc;
use tower::ServiceExt;

const APPROVE: &str = "/distributed/handoffs/abc123/approve?workspace=default";
const REVOKE: &str = "/distributed/handoffs/abc123/revoke?workspace=default";
const RECOVER: &str = "/distributed/claims/claim1/recover?workspace=default";

#[allow(clippy::await_holding_lock)]
async fn with_caller_env<'a, T>(
    vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _env = orbit_common::test_env::scoped(vars);
    fut.await
}

/// Resolve as an explicit operator. The override outranks every other signal.
async fn as_operator<T>(fut: impl std::future::Future<Output = T>) -> T {
    with_caller_env([(OPERATOR_OVERRIDE_ENV, Some("1"))], fut).await
}

/// A replica checkout: coordination writes belong to another machine.
fn replica_state() -> DashboardState {
    DashboardState::single(Arc::new(
        OrbitRuntime::in_memory()
            .expect("build in-memory runtime")
            .with_coordination_write_owner(Some("owner-machine".to_string())),
    ))
}

async fn get(state: DashboardState, uri: &str) -> axum::response::Response {
    router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header("origin", "http://localhost:7878")
                .header("host", "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn post(state: DashboardState, uri: &str, body: &str) -> axum::response::Response {
    router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header("origin", "http://localhost:7878")
                .header("host", "localhost:7878")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

fn approve_body() -> String {
    r#"{"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","request_id":"r1"}"#
        .to_string()
}

fn revoke_body() -> String {
    r#"{"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","reason":"withdrawn","request_id":"r1"}"#
        .to_string()
}

fn recover_body() -> String {
    r#"{"expected_phase":"running","status":"blocked","reason":"host lost","request_id":"r1"}"#
        .to_string()
}

/// A replica holds no claim state. The read says so instead of erroring; every
/// write is refused with the operator capability intact — replica is a
/// destination fact, not a missing permission.
#[tokio::test]
async fn a_replica_checkout_reads_empty_and_refuses_every_owner_action() {
    let response = as_operator(get(
        replica_state(),
        "/distributed/claims?workspace=default",
    ))
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: Value = body_json(response).await;
    assert_eq!(json["owner_workspace"], false);
    assert_eq!(json["refusal"], "replica_checkout");
    assert_eq!(json["claims"].as_array().expect("claims").len(), 0);

    for (uri, body) in [
        (APPROVE, approve_body()),
        (REVOKE, revoke_body()),
        (RECOVER, recover_body()),
    ] {
        let response = as_operator(post(replica_state(), uri, &body)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
        let json: Value = body_json(response).await;
        assert_eq!(json["code"], "replica_checkout", "{uri}");
    }
}
