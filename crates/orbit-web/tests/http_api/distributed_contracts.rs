use super::support::{Fixture, Server, error_code, isolated};
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
            steering: None,
            leaf_run_evidence: None,
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

fn edit_registry(
    fixture: &Fixture,
    edit: impl FnOnce(&mut orbit_types::workspace::WorkspaceRegistry),
) {
    let path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry = orbit_registry::workspace_registry::load_registry_from(&path).unwrap();
    edit(&mut registry);
    orbit_registry::workspace_registry::save_registry_to(&registry, &path).unwrap();
}

#[test]
fn retry_handoff_inactive_workspace_is_typed_workspace_unavailable() {
    isolated(
        "distributed_contracts::retry_handoff_inactive_workspace_is_typed_workspace_unavailable",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let body = serde_json::json!({"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","expected_latest_attempt":1,"expected_latest_state":"stopped","reason":"retry safely","request_key":"r1"});
            let uri = "/api/distributed/handoffs/abc123/retry?workspace=ws_http_fixture";
            // The selected workspace becomes inactive (its checkout moved)
            // before the replay. The Ws extractor answers before the retry
            // handler and its receipt lookup, so the refusal is environmental:
            // it stays a 400 but must carry the typed code the browser uses
            // to retain a retained request_key.
            let registry_path =
                orbit_registry::workspace_registry::registry_path_for(&fixture.global);
            let original_registry = std::fs::read(&registry_path).unwrap();
            edit_registry(&fixture, |registry| {
                registry.checkouts[0].repo_root = fixture.path("missing-checkout");
            });
            let refused = error_code(
                server.send("POST", uri, body.clone()),
                400,
                "workspace_unavailable",
            );
            assert!(
                refused["error"]
                    .as_str()
                    .is_some_and(|message| message.contains("is inactive")),
                "{refused}"
            );
            // After the workspace is active again the same replay is no longer
            // refused as workspace_unavailable; it reaches the handler.
            std::fs::write(&registry_path, &original_registry).unwrap();
            let restored = server.send("POST", uri, body);
            let restored_body: Value = restored.json().unwrap();
            assert_ne!(
                restored_body.get("code"),
                Some(&serde_json::json!("workspace_unavailable")),
                "{restored_body}"
            );
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
                    "/distributed/handoffs/abc123/retry",
                    serde_json::json!({"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","expected_latest_attempt":1,"expected_latest_state":"stopped","reason":"retry safely","request_key":"r1"}),
                ),
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

/// Each operator route refuses a bad reason or replay key with a 400 `{error}`
/// from the core operation, before any receipt is written. The 2000-byte
/// reason and 128-byte key are the accepted boundary; they pass the bounds
/// and fail later for the absent handoff or claim.
#[test]
fn operator_handoff_reason_and_replay_key_are_bounded_on_every_route() {
    isolated(
        "distributed_contracts::operator_handoff_reason_and_replay_key_are_bounded_on_every_route",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            fn retry_body(reason: &str, key: &str) -> Value {
                serde_json::json!({"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","expected_latest_attempt":1,"expected_latest_state":"stopped","reason":reason,"request_key":key})
            }
            fn revoke_body(reason: &str, key: &str) -> Value {
                serde_json::json!({"expected_candidate_commit":"aaaa","expected_base_commit":"bbbb","reason":reason,"request_id":key})
            }
            fn recover_body(reason: &str, key: &str) -> Value {
                serde_json::json!({"expected_phase":"running","status":"blocked","reason":reason,"request_id":key})
            }
            type BodyFor = fn(&str, &str) -> Value;
            let routes: [(&str, BodyFor); 3] = [
                (
                    "/api/distributed/handoffs/abc123/retry?workspace=ws_http_fixture",
                    retry_body,
                ),
                (
                    "/api/distributed/handoffs/abc123/revoke?workspace=ws_http_fixture",
                    revoke_body,
                ),
                (
                    "/api/distributed/claims/claim1/recover?workspace=ws_http_fixture",
                    recover_body,
                ),
            ];
            let long_reason = "r".repeat(2001);
            let long_key = "k".repeat(129);
            for (uri, body) in routes {
                for (reason, key) in [
                    (long_reason.as_str(), "ok-key"),
                    ("retry safely", "has space"),
                    ("retry safely", long_key.as_str()),
                ] {
                    let response = server.send("POST", uri, body(reason, key));
                    assert_eq!(response.status(), 400, "{uri} {reason:.12} {key}");
                    let json: Value = response.json().unwrap();
                    assert!(json["error"].is_string(), "{uri}: {json}");
                }
                // The boundary values pass the bounds and fail later, on the
                // absent handoff or claim, not on the reason or key.
                let boundary = server.send("POST", uri, body(&"r".repeat(2000), &"k".repeat(128)));
                let json: Value = boundary.json().unwrap();
                assert!(
                    !json["error"].as_str().unwrap_or_default().contains("bytes"),
                    "{uri}: {json}"
                );
            }
        },
    );
}
