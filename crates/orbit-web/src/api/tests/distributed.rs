//! The distributed claim provenance and owner handoff endpoints [ORB-12516].
//!
//! These cover the HTTP boundary on its own terms: capability, workspace
//! selection, request shape and the typed refusal codes a client acts on. The
//! lifecycle decisions behind them — what a stale candidate, an unresolved
//! merge intent or a replica checkout does to the store — are covered against
//! the real coordination store in `orbit_core::application::review`, so nothing
//! here re-implements them.
//!
//! No live host is touched: every case runs against an in-memory runtime.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_common::governance::authorization::{
    DASHBOARD_CLAIM_RECOVER, DASHBOARD_HANDOFF_APPROVE, DASHBOARD_HANDOFF_REVOKE,
    OPERATOR_OVERRIDE_ENV,
};
use orbit_core::OrbitRuntime;
use serde_json::Value;
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;
use crate::state::DashboardState;

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

/// Resolve as an agent: override cleared, agent envelope declared so
/// resolution stops before the interactive-terminal probe a local `cargo test`
/// would otherwise satisfy.
async fn as_agent<T>(fut: impl std::future::Future<Output = T>) -> T {
    with_caller_env(
        [
            (OPERATOR_OVERRIDE_ENV, None),
            ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
        ],
        fut,
    )
    .await
}

fn state() -> DashboardState {
    DashboardState::single(Arc::new(
        OrbitRuntime::in_memory().expect("build in-memory runtime"),
    ))
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

const APPROVE: &str = "/distributed/handoffs/abc123/approve?workspace=default";
const REVOKE: &str = "/distributed/handoffs/abc123/revoke?workspace=default";
const RECOVER: &str = "/distributed/claims/claim1/recover?workspace=default";

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

#[tokio::test]
async fn the_claim_read_reports_its_schema_and_the_actions_this_session_could_take() {
    let response = as_operator(get(state(), "/distributed/claims?workspace=default")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: Value = body_json(response).await;

    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["owner_workspace"], true);
    // The incomplete routed feature stays gated; the read says so rather than
    // implying the dashboard can drive a distributed drain.
    assert_eq!(json["distributed_execution_enabled"], false);
    assert_eq!(json["claims"].as_array().expect("claims").len(), 0);
    assert_eq!(json["capabilities"]["handoff_approve"]["authorized"], true);
    assert_eq!(json["capabilities"]["handoff_revoke"]["authorized"], true);
    assert_eq!(json["capabilities"]["claim_recover"]["authorized"], true);
}

/// The projection is the same decision the mutation enforces, not a second
/// opinion: an agent session is told it cannot act, and is also refused when
/// it tries anyway.
#[tokio::test]
async fn an_agent_session_is_told_it_cannot_act_and_is_refused_when_it_acts_anyway() {
    let projected = as_agent(get(state(), "/distributed/claims?workspace=default")).await;
    let json: Value = body_json(projected).await;
    assert_eq!(json["capabilities"]["handoff_approve"]["authorized"], false);
    assert!(
        json["capabilities"]["handoff_approve"]["reason"]
            .as_str()
            .expect("reason")
            .contains("operator"),
        "{json}"
    );

    for (uri, body, operation) in [
        (APPROVE, approve_body(), DASHBOARD_HANDOFF_APPROVE.id),
        (REVOKE, revoke_body(), DASHBOARD_HANDOFF_REVOKE.id),
        (RECOVER, recover_body(), DASHBOARD_CLAIM_RECOVER.id),
    ] {
        let response = as_agent(post(state(), uri, &body)).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
        let json: Value = body_json(response).await;
        assert_eq!(json["code"], "authorization_denied", "{uri}");
        assert_eq!(json["operation"], operation, "{uri}");
    }
}

/// Every owner action names one concrete workspace. An unselected aggregate
/// view has no claim state to act on, and guessing one would act on the wrong
/// workspace's claim.
#[tokio::test]
async fn owner_actions_require_an_explicit_workspace() {
    for (uri, body) in [
        ("/distributed/handoffs/abc123/approve", approve_body()),
        ("/distributed/handoffs/abc123/revoke", revoke_body()),
        ("/distributed/claims/claim1/recover", recover_body()),
    ] {
        let response = as_operator(post(state(), uri, &body)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        let json: Value = body_json(response).await;
        assert_eq!(json["code"], "workspace_required", "{uri}");
    }
}

/// The replay identity is what makes a retried decision idempotent rather than
/// a second authorization, so it is required rather than defaulted.
#[tokio::test]
async fn a_decision_without_a_replay_identity_is_refused() {
    let response = as_operator(post(
        state(),
        APPROVE,
        r#"{"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","request_id":"  "}"#,
    ))
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json: Value = body_json(response).await;
    assert!(
        json["error"].as_str().expect("error").contains("retry"),
        "{json}"
    );
}

#[tokio::test]
async fn revocation_and_recovery_require_a_reason_and_a_permitted_target() {
    let blank = as_operator(post(
        state(),
        REVOKE,
        r#"{"expected_candidate_commit":"a","expected_base_commit":"b","reason":"   ","request_id":"r1"}"#,
    ))
    .await;
    assert_eq!(blank.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(blank).await["error"]
            .as_str()
            .expect("error")
            .contains("reason")
    );

    // Recovery moves a task to blocked for diagnosis or backlog for retry.
    // Nothing else is a recovery outcome, least of all done.
    let done = as_operator(post(
        state(),
        RECOVER,
        r#"{"expected_phase":"running","status":"done","reason":"ship it","request_id":"r1"}"#,
    ))
    .await;
    assert_eq!(done.status(), StatusCode::BAD_REQUEST);
    assert!(
        body_json(done).await["error"]
            .as_str()
            .expect("error")
            .contains("backlog")
    );
}

/// A handoff or claim the owner does not hold is a conflict the client fixes by
/// refreshing, carrying the code that says so.
#[tokio::test]
async fn acting_on_a_handoff_the_owner_does_not_hold_is_a_refreshable_conflict() {
    for (uri, body) in [
        (APPROVE, approve_body()),
        (REVOKE, revoke_body()),
        (RECOVER, recover_body()),
    ] {
        let response = as_operator(post(state(), uri, &body)).await;
        assert_eq!(response.status(), StatusCode::CONFLICT, "{uri}");
        let json: Value = body_json(response).await;
        assert!(
            json["code"] == "handoff_not_current" || json["code"] == "stale_claim",
            "{uri}: {json}"
        );
        assert!(json["remedy"].as_str().expect("remedy").contains("refresh"));
    }
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

/// Path ids are validated before anything reads the store, so a traversal or a
/// control character never reaches a lookup.
#[tokio::test]
async fn malformed_identifiers_are_rejected_before_any_lookup() {
    let response = as_operator(post(
        state(),
        "/distributed/handoffs/..%2Fetc/approve?workspace=default",
        &approve_body(),
    ))
    .await;
    assert!(
        response.status() == StatusCode::BAD_REQUEST || response.status() == StatusCode::NOT_FOUND,
        "{:?}",
        response.status()
    );
}
