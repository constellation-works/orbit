//! Tests for the routine-health JSON API [ORB-10138].

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::application::routines::ClockStatus;
use orbit_core::{OrbitRuntime, RoutineFireRecord, RoutineFireState};
use orbit_registry::{NewHostIdentity, ensure_host_identity};
use tower::ServiceExt;

use super::super::router;
use super::super::routines::{clock_json, duration_ms, fire_json, fire_ok};
use super::test_support::body_json;
use crate::state::DashboardState;

fn fire(state: RoutineFireState, created_at: &str, updated_at: &str) -> RoutineFireRecord {
    RoutineFireRecord {
        routine_name: "ship-sweep".to_string(),
        slot: "2026-07-11T22:00:00+00:00".to_string(),
        attempt: 1,
        state,
        run_id: Some("run-1".to_string()),
        source_workspace: "polaris".to_string(),
        detail: None,
        created_at: created_at.to_string(),
        updated_at: updated_at.to_string(),
    }
}

#[test]
fn fire_ok_classifies_terminal_and_in_flight_states() {
    assert_eq!(fire_ok(RoutineFireState::Succeeded), Some(true));
    assert_eq!(fire_ok(RoutineFireState::Failed), Some(false));
    assert_eq!(fire_ok(RoutineFireState::TimedOut), Some(false));
    assert_eq!(fire_ok(RoutineFireState::Error), Some(false));
    assert_eq!(fire_ok(RoutineFireState::Intent), None);
    assert_eq!(fire_ok(RoutineFireState::Dispatched), None);
}

#[test]
fn duration_ms_spans_intent_to_terminal() {
    let f = fire(
        RoutineFireState::Succeeded,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:12.500+00:00",
    );
    assert_eq!(duration_ms(&f), Some(12_500));
}

#[test]
fn duration_ms_is_none_while_in_flight() {
    let f = fire(
        RoutineFireState::Dispatched,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:05+00:00",
    );
    assert_eq!(duration_ms(&f), None);
}

#[test]
fn fire_json_surfaces_outcome_and_finish() {
    let f = fire(
        RoutineFireState::Succeeded,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:10+00:00",
    );
    let json = fire_json(&f);
    assert_eq!(json["state"], "succeeded");
    assert_eq!(json["ok"], true);
    assert_eq!(json["duration_ms"], 10_000);
    assert_eq!(json["finished_at"], "2026-07-11T22:00:10+00:00");
    assert_eq!(json["run_id"], "run-1");
}

#[test]
fn fire_json_omits_finish_while_in_flight() {
    let f = fire(
        RoutineFireState::Dispatched,
        "2026-07-11T22:00:00+00:00",
        "2026-07-11T22:00:00+00:00",
    );
    let json = fire_json(&f);
    assert!(json["ok"].is_null());
    assert!(json["finished_at"].is_null());
    assert!(json["duration_ms"].is_null());
}

#[test]
fn clock_json_keeps_service_state_and_health_distinct() {
    let healthy = clock_json(&ClockStatus {
        configured_cadence_seconds: 300,
        effective_cadence_seconds: Some(300),
        enabled: true,
        loaded: true,
        running: Some(true),
        schedulable: true,
        health_issue: None,
        last_tick_at: Some("previous".to_string()),
        next_tick_at: Some("next".to_string()),
        platform: "systemd",
    });
    assert_eq!(healthy["health"], "healthy");
    assert_eq!(healthy["enabled"], true);
    assert_eq!(healthy["running"], true);
    assert_eq!(healthy["next_tick_at"], "next");

    let missed = clock_json(&ClockStatus {
        configured_cadence_seconds: 300,
        effective_cadence_seconds: None,
        enabled: true,
        loaded: true,
        running: Some(false),
        schedulable: false,
        health_issue: Some("no future trigger".to_string()),
        last_tick_at: None,
        next_tick_at: None,
        platform: "systemd",
    });
    assert_eq!(missed["health"], "missed");
    assert_eq!(missed["enabled"], true, "enabled is not health");
    assert_eq!(missed["running"], false);
}

/// End-to-end: the endpoint resolves host-level routine state from the global
/// root and returns a well-formed envelope even when no routines are
/// configured (empty registry). Exercises the wiring, not fixture data.
#[tokio::test]
async fn routines_endpoint_returns_envelope_for_empty_host() {
    let temp = tempfile::tempdir().expect("temp global root");
    ensure_host_identity(temp.path(), || {
        Ok(NewHostIdentity {
            host_id: "dashboard-test".to_string(),
            task_prefix: "DA".to_string(),
        })
    })
    .expect("seed host identity");
    let state = DashboardState::global(temp.path().to_path_buf(), Vec::new(), None);

    let response = router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/routines")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert!(json["generated_at"].is_string());
    assert!(json["host_id"].is_string());
    assert!(json["clock"]["provider"].is_string());
    assert!(json["clock"]["configured_cadence_seconds"].is_number());
    assert!(json["clock"]["enabled"].is_boolean());
    assert_eq!(json["routines"], serde_json::json!([]));
    assert_eq!(json["load_errors"], serde_json::json!([]));
}

/// Pin the process signals `CallerCapabilities::resolve` reads, for the whole
/// request rather than just its construction.
///
/// The guard in `orbit_common::test_env` is process-wide, so it serializes two
/// tests only when *both* take it. Parallel `as_operator` tests in
/// `api::tests::auto_tasks`, `api::tests::runs`, and `api::tests::operation`
/// set `ORBIT_OPERATOR=1` under that lock. This case used to read the variable
/// without holding it, so a sibling could promote the caller to Operator;
/// authorization then succeeded and `routine_statuses` on
/// `DashboardState::single`'s empty global root mapped `InvalidInput` to HTTP
/// 400 instead of the 403 unidentified-caller denial [ORB-10894].
#[allow(clippy::await_holding_lock)]
async fn with_caller_env<'a, T>(
    vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let _env = orbit_common::test_env::scoped(vars);
    fut.await
}

#[tokio::test]
async fn routine_mutation_denies_an_unidentified_dashboard_caller() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let state = DashboardState::single(Arc::new(runtime));
    // Agent envelope is *set* rather than cleared: with nothing declared,
    // resolution falls through to the interactive-terminal probe, and a TTY
    // `cargo test` would resolve to Operator for a reason unrelated to the
    // handler. The empty-grants unidentified branch is covered without process
    // state in `orbit_common::governance::tests::authorization`.
    let response = with_caller_env(
        [
            (OPERATOR_OVERRIDE_ENV, None),
            ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
            ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
        ],
        async {
            router()
                .with_state(state)
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/routines/toggle?workspace=default")
                        .header("origin", "http://localhost:7878")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            r#"{"name":"nightly","source":"default","target":"job:nightly","host_id":"host-a","expected_enabled":true,"enabled":false}"#,
                        ))
                        .expect("request"),
                )
                .await
                .expect("response")
        },
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let json = body_json(response).await;
    assert_eq!(json["code"], "authorization_denied");
    assert_eq!(json["operation"], "routine.toggle");
}

#[tokio::test]
async fn operations_mutations_require_an_explicit_workspace() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let state = DashboardState::single(Arc::new(runtime));
    let response = router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/routines/clock")
                .header("origin", "http://localhost:7878")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"action":"disable","host_id":"host-a","expected_enabled":true,"expected_cadence_seconds":60}"#,
                ))
                .expect("request"),
        )
        .await
        .expect("response");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert_eq!(json["code"], "workspace_required");
}
