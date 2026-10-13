use super::support::{Fixture, Server, isolated};
use axum::http::StatusCode;
use chrono::Utc;
use orbit_core::OrbitRuntime;
use orbit_store::contracts::{
    AdmissionRunContext, ClaimInspection, ExecutionClaim, ExecutionClaimPhase,
};
use orbit_types::task::ExecutionLocation;
use serde_json::Value;
fn owner_state_with_claims(
    runtime: &OrbitRuntime,
    claims: impl IntoIterator<Item = (String, String, ExecutionClaimPhase)>,
) {
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
}
#[test]
fn claim_console_filters_by_task_and_state_and_compacts_settled_rows() {
    isolated(
        "distributed_contracts::claim_console_filters_by_task_and_state_and_compacts_settled_rows",
        || {
            let fixture = Fixture::new();
            owner_state_with_claims(
                &fixture.runtime,
                [
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
                ],
            );
            let server = fixture.server(true);

            let default: Value = body_json(get(&server, "/distributed/claims?workspace=default"));
            let default_claims = default["claims"].as_array().expect("claims");
            assert_eq!(default_claims.len(), 2, "the default is unsettled claims");
            assert!(
                default_claims
                    .iter()
                    .all(|claim| claim["unsettled"] == true)
            );

            let task_filtered: Value = body_json(get(
                &server,
                "/distributed/claims?workspace=default&task=ORB-1",
            ));
            let task_claims = task_filtered["claims"].as_array().expect("claims");
            assert_eq!(task_claims.len(), 1);
            assert_eq!(task_claims[0]["task_id"], "ORB-1");

            let settled: Value = body_json(get(
                &server,
                "/distributed/claims?workspace=default&state=settled&task=ORB-1",
            ));
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

            let all: Value = body_json(get(
                &server,
                "/distributed/claims?workspace=default&state=all",
            ));
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

            let detailed: Value = body_json(get(
                &server,
                "/distributed/claims?workspace=default&state=settled&detail=true",
            ));
            let detailed_claims = detailed["claims"].as_array().expect("claims");
            assert_eq!(detailed_claims.len(), 3);
            assert!(
                detailed_claims
                    .iter()
                    .all(|claim| claim["footprint"].is_array())
            );
        },
    );
}

#[test]
fn default_claim_response_stays_small_with_long_settled_history() {
    isolated(
        "distributed_contracts::default_claim_response_stays_small_with_long_settled_history",
        || {
            let fixture = Fixture::new();
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
            owner_state_with_claims(&fixture.runtime, claims);
            let server = fixture.server(true);

            let response = get(&server, "/distributed/claims?workspace=default");
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.bytes().expect("response body");
            assert!(
                bytes.len() < 50_000,
                "default response is {} bytes",
                bytes.len()
            );
            let json: Value = serde_json::from_slice(&bytes).expect("JSON response");
            assert_eq!(json["claims"].as_array().expect("claims").len(), 5);
        },
    );
}

fn get(server: &Server, uri: &str) -> reqwest::blocking::Response {
    server.get(&format!(
        "/api{}",
        uri.replace("workspace=default", "workspace=ws_http_fixture")
    ))
}
fn body_json(response: reqwest::blocking::Response) -> Value {
    response.json().unwrap()
}
#[test]
fn a_replica_checkout_reads_empty_and_refuses_every_owner_action() {
    isolated(
        "distributed_contracts::a_replica_checkout_reads_empty_and_refuses_every_owner_action",
        || {
            let fixture = Fixture::replica_of("hm_fixture_remote");
            let server = fixture.server(true);
            let response = get(&server, "/distributed/claims?workspace=default");
            assert_eq!(response.status(), StatusCode::OK);
            let json = body_json(response);
            assert_eq!(json["owner_workspace"], false);
            assert_eq!(json["refusal"], "replica_checkout");
            assert_eq!(json["claims"].as_array().unwrap().len(), 0);
            for (uri, body) in [
                (
                    "/distributed/handoffs/abc123/approve",
                    serde_json::json!({"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","request_id":"r1"}),
                ),
                (
                    "/distributed/handoffs/abc123/revoke",
                    serde_json::json!({"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","reason":"withdrawn","request_id":"r1"}),
                ),
                (
                    "/distributed/claims/claim1/recover",
                    serde_json::json!({"expected_phase":"running","status":"blocked","reason":"host lost","request_id":"r1"}),
                ),
            ] {
                let response = server.send(
                    "POST",
                    &format!("/api{uri}?workspace=ws_http_fixture"),
                    body,
                );
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{uri}");
                assert_eq!(body_json(response)["code"], "replica_checkout", "{uri}");
            }
        },
    );
}
