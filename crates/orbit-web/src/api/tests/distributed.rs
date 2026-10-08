use super::super::router;
use super::test_support::{body_json, enter_isolated_child};
use crate::state::DashboardState;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;
use orbit_core::OrbitRuntime;
use orbit_store::contracts::{
    AdmissionRunContext, ClaimInspection, ExecutionClaim, ExecutionClaimPhase,
};
use orbit_types::task::ExecutionLocation;
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

fn owner_state_with_claims(
    claims: impl IntoIterator<Item = (String, String, ExecutionClaimPhase)>,
) -> DashboardState {
    let runtime = OrbitRuntime::in_memory().expect("build in-memory runtime");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    let connection = runtime.sqlite_store().expect("sqlite store").connection();
    let connection = connection.lock().expect("sqlite connection");
    for (claim_id, task_id, phase) in claims {
        let now = Utc::now().to_rfc3339();
        let claim = ExecutionClaim {
            claim_id,
            task_id,
            request_id: "request-fixture".into(),
            executed_on: ExecutionLocation {
                machine_id: "hm_fixture".into(),
                machine_name: Some("fixture-host".into()),
            },
            run_context: AdmissionRunContext {
                run_id: "run-fixture".into(),
                job_name: "task_claimed_pipeline".into(),
                machine_name: Some("fixture-host".into()),
            },
            footprint: vec!["src/main.rs".into()],
            reservation_id: "reservation-fixture".into(),
            reservation_expires_at: now.clone(),
            phase,
            repair: None,
        };
        let inspection = ClaimInspection {
            claim: claim.clone(),
            bound_run: None,
            created_at: now.clone(),
            updated_at: now.clone(),
            last_event: if phase.is_unsettled() {
                "claimed".into()
            } else {
                "settled".into()
            },
            age_seconds: Some(0),
            unresolved_merge_intent: None,
            landing_invalidated: false,
            release: None,
            preserved_candidate: None,
            settlement: None,
        };
        let claim_json = serde_json::to_string(&claim).expect("serialize claim fixture");
        let inspection_json =
            serde_json::to_string(&inspection).expect("serialize claim inspection fixture");
        for (kind, payload) in [
            ("distributed-execution-claim-v1", claim_json),
            ("distributed-claim-lifecycle-v1", inspection_json),
        ] {
            connection
                .execute(
                    "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    (
                        workspace_id.as_str(),
                        kind,
                        claim.claim_id.as_str(),
                        payload.as_str(),
                        "api-test-fixture",
                        now.as_str(),
                    ),
                )
                .expect("insert claim fixture row");
        }
    }
    drop(connection);
    DashboardState::single(Arc::new(runtime))
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

#[tokio::test]
async fn claim_console_filters_by_task_and_state_and_compacts_settled_rows() {
    if !enter_isolated_child(
        module_path!(),
        "claim_console_filters_by_task_and_state_and_compacts_settled_rows",
    ) {
        return;
    }
    let state = owner_state_with_claims([
        (
            "active-1".into(),
            "ORB-1".into(),
            ExecutionClaimPhase::Running,
        ),
        (
            "active-2".into(),
            "ORB-2".into(),
            ExecutionClaimPhase::HandedOff,
        ),
        (
            "settled-1".into(),
            "ORB-1".into(),
            ExecutionClaimPhase::Failed,
        ),
        (
            "settled-2".into(),
            "ORB-1".into(),
            ExecutionClaimPhase::Revoked,
        ),
        (
            "settled-3".into(),
            "ORB-2".into(),
            ExecutionClaimPhase::Landed,
        ),
    ]);

    let default: Value =
        body_json(get(state.clone(), "/distributed/claims?workspace=default").await).await;
    let default_claims = default["claims"].as_array().expect("claims");
    assert_eq!(default_claims.len(), 2, "the default is unsettled claims");
    assert!(
        default_claims
            .iter()
            .all(|claim| claim["unsettled"] == true)
    );

    let task_filtered: Value = body_json(
        get(
            state.clone(),
            "/distributed/claims?workspace=default&task=ORB-1",
        )
        .await,
    )
    .await;
    let task_claims = task_filtered["claims"].as_array().expect("claims");
    assert_eq!(task_claims.len(), 1);
    assert_eq!(task_claims[0]["task_id"], "ORB-1");

    let settled: Value = body_json(
        get(
            state.clone(),
            "/distributed/claims?workspace=default&state=settled&task=ORB-1",
        )
        .await,
    )
    .await;
    let settled_claims = settled["claims"].as_array().expect("claims");
    assert_eq!(settled_claims.len(), 2);
    for claim in settled_claims {
        assert_eq!(claim.as_object().expect("summary").len(), 5);
        assert!(claim["claim_id"].is_string());
        assert_eq!(claim["task_id"], "ORB-1");
        assert_eq!(claim["host"], "fixture-host");
        assert!(claim["outcome"].is_string());
        assert!(claim["settled_at"].is_string());
    }

    let all: Value = body_json(
        get(
            state.clone(),
            "/distributed/claims?workspace=default&state=all",
        )
        .await,
    )
    .await;
    let all_claims = all["claims"].as_array().expect("claims");
    assert_eq!(all_claims.len(), 5);
    assert_eq!(
        all_claims
            .iter()
            .filter(|claim| claim.as_object().is_some_and(|row| row.len() == 5))
            .count(),
        3
    );
    assert!(
        all_claims
            .iter()
            .filter(|claim| claim["unsettled"] == true)
            .all(|claim| claim["run_context"].is_object())
    );

    let detailed: Value = body_json(
        get(
            state,
            "/distributed/claims?workspace=default&state=settled&detail=true",
        )
        .await,
    )
    .await;
    let detailed_claims = detailed["claims"].as_array().expect("claims");
    assert_eq!(detailed_claims.len(), 3);
    assert!(
        detailed_claims
            .iter()
            .all(|claim| claim["footprint"].is_array())
    );
}

#[tokio::test]
async fn default_claim_response_stays_small_with_long_settled_history() {
    if !enter_isolated_child(
        module_path!(),
        "default_claim_response_stays_small_with_long_settled_history",
    ) {
        return;
    }
    let claims = (0..500)
        .map(|index| {
            (
                format!("settled-{index:04}"),
                "ORB-1".into(),
                ExecutionClaimPhase::Failed,
            )
        })
        .chain((0..5).map(|index| {
            (
                format!("active-{index}"),
                "ORB-1".into(),
                ExecutionClaimPhase::Running,
            )
        }));
    let state = owner_state_with_claims(claims);

    let response = get(state, "/distributed/claims?workspace=default").await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    assert!(
        bytes.len() < 50_000,
        "default response is {} bytes",
        bytes.len()
    );
    let json: Value = serde_json::from_slice(&bytes).expect("JSON response");
    assert_eq!(json["claims"].as_array().expect("claims").len(), 5);
}
