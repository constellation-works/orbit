use orbit_core::TaskStatus;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_engine::RuntimeHost;
use orbit_types::workflow::OwedCriterion;
use serde_json::json;

use super::support::{Fixture, error_code, isolated, json_ok};

const WS: &str = "?workspace=ws_http_fixture";

fn plain_task(fixture: &Fixture, title: &str) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "operator record fixture".into(),
            acceptance_criteria: vec!["The fixture is observable.".into()],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn seed_pilot_receipt(fixture: &Fixture, id: &str, audit: serde_json::Value) {
    fixture
        .runtime
        .update_task_as_human(
            id,
            TaskUpdateParams {
                comment: Some(format!("operation_id={}\n{}", "a".repeat(64), audit)),
                ..Default::default()
            },
            "task-pilot".to_string(),
        )
        .expect("record pilot receipt");
}

fn held_task(fixture: &Fixture) -> String {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Held validation work".into(),
            description: "Repair the fixture.".into(),
            acceptance_criteria: vec![
                "Run the live evaluation with `orbit.pipeline.invoke`.".into(),
            ],
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap()
        .id;
    seed_pilot_receipt(
        fixture,
        &id,
        json!({"assessment": {
            "task_id": id,
            "disposition": "selectors",
            "confidence": "high",
        }}),
    );
    id
}

fn owed_task(fixture: &Fixture) -> String {
    let criteria: Vec<String> = vec![
        "First criterion.".into(),
        "Measure p95 on the deployed build.".into(),
    ];
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Merged with owed criterion".into(),
            description: "operator record fixture".into(),
            acceptance_criteria: criteria.clone(),
            status: Some(TaskStatus::Review),
            ..Default::default()
        })
        .unwrap()
        .id;
    let digest = orbit_common::security::release::sha256_hex(
        serde_json::json!(criteria).to_string().as_bytes(),
    );
    seed_pilot_receipt(
        fixture,
        &id,
        json!({
            "assessment": {"task_id": id, "disposition": "selectors", "confidence": "high"},
            "post_deploy": {
                "criteria": digest,
                "requirements": [{"criterion": 2, "host": "deploy-host", "command": "measure p95"}],
            },
        }),
    );
    RuntimeHost::admit_owed_criteria(
        &fixture.runtime,
        &id,
        "run-fixture",
        &[OwedCriterion {
            criterion: 2,
            host: "deploy-host".into(),
            command: "measure p95".into(),
            target: "p95 <= 150 ms".into(),
            candidate_evidence: String::new(),
        }],
    )
    .expect("admit owed criterion");
    id
}

#[test]
fn operator_record_routes_gate_refusals_and_success() {
    isolated(
        "task_operator_records::operator_record_routes_gate_refusals_and_success",
        || {
            let fixture = Fixture::new();
            let held = held_task(&fixture);
            let plain = plain_task(&fixture, "plain");
            let owed = owed_task(&fixture);
            let pilot = |id: &str| format!("/api/tasks/{id}/pilot-admission{WS}");
            let deploy = |id: &str| format!("/api/tasks/{id}/post-deploy{WS}");

            // Without operator session: same 403 as triage/recovery.
            let anon = fixture.server(false);
            error_code(
                anon.send(
                    "POST",
                    &pilot(&held),
                    json!({"verdict":"clear","evidence":"checked"}),
                ),
                403,
                "authorization_denied",
            );
            error_code(
                anon.send(
                    "POST",
                    &deploy(&owed),
                    json!({"criterion":2,"evidence":"measured"}),
                ),
                403,
                "authorization_denied",
            );

            let operator = fixture.server(true);
            let assert_hold = |id: &str, expected: bool| {
                let record = json_ok(operator.get(&format!("/api/tasks/{id}{WS}")));
                assert_eq!(record["status"], "backlog");
                assert_eq!(record["pilot_warnings"], json!([]));
                assert_eq!(record["resolvable_pilot_admission_hold"], expected);
            };
            // Admission-held backlog records must advertise controls even
            // though their promotion-only warning array is empty.
            assert_hold(&held, true);
            assert_hold(&plain, false);
            // Empty evidence refused, nothing written.
            let held_history = fixture.runtime.get_task_history(&held).unwrap().len();
            error_code(
                operator.send(
                    "POST",
                    &pilot(&held),
                    json!({"verdict":"clear","evidence":"  "}),
                ),
                400,
                "pilot_admission_evidence_required",
            );
            error_code(
                operator.send("POST", &deploy(&owed), json!({"criterion":2})),
                400,
                "post_deploy_evidence_required",
            );
            assert_eq!(
                fixture.runtime.get_task_history(&held).unwrap().len(),
                held_history
            );

            // No matching hold / criterion that does not exist: 409, no write.
            let plain_history = fixture.runtime.get_task_history(&plain).unwrap().len();
            error_code(
                operator.send(
                    "POST",
                    &pilot(&plain),
                    json!({"verdict":"clear","evidence":"checked"}),
                ),
                409,
                "pilot_admission_no_matching_hold",
            );
            let owed_history = fixture.runtime.get_task_history(&owed).unwrap().len();
            for criterion in [0, 1] {
                error_code(
                    operator.send(
                        "POST",
                        &deploy(&owed),
                        json!({"criterion":criterion,"evidence":"measured"}),
                    ),
                    409,
                    "post_deploy_criterion_not_owed",
                );
            }
            error_code(
                operator.send(
                    "POST",
                    &deploy(&plain),
                    json!({"criterion":2,"evidence":"measured"}),
                ),
                409,
                "post_deploy_criterion_not_owed",
            );
            assert_eq!(
                fixture.runtime.get_task_history(&owed).unwrap().len(),
                owed_history
            );
            assert_eq!(
                fixture.runtime.get_task_history(&plain).unwrap().len(),
                plain_history
            );

            // Clear hold: human comment core accepts, resolution recorded.
            let resp = operator.send(
                "POST",
                &pilot(&held),
                json!({"verdict":"clear","evidence":"Operator ran the live evaluation."}),
            );
            assert_eq!(resp.status().as_u16(), 200, "{}", resp.text().unwrap());
            let history = fixture.runtime.get_task_history(&held).unwrap();
            assert!(
                history
                    .iter()
                    .any(|e| e.event == "operator_validation_resolved" && e.by == "dashboard"),
                "history should show operator_validation_resolved by dashboard: {history:?}"
            );
            let comments = fixture.runtime.get_task_comments(&held).unwrap();
            assert!(
                comments.iter().any(|c| c.message
                    == "task-pilot-admission: clear\nOperator ran the live evaluation."),
                "comments should carry the exact grammar: {comments:?}"
            );
            assert_hold(&held, false);

            // Approve anyway: same route, override verdict.
            let held2 = held_task(&fixture);
            let resp = operator.send(
                "POST",
                &pilot(&held2),
                json!({"verdict":"approve-anyway","evidence":"Operator accepts the risk."}),
            );
            assert_eq!(resp.status().as_u16(), 200, "{}", resp.text().unwrap());
            let history = fixture.runtime.get_task_history(&held2).unwrap();
            assert!(
                history
                    .iter()
                    .any(|e| e.event == "operator_validation_resolved"),
                "approve-anyway should resolve the hold: {history:?}"
            );
            assert_hold(&held2, false);

            // Native-OS admission holds use the same public read/action pair.
            // This fixture tests the record, not a native host's capability.
            for verdict in ["clear", "approve-anyway"] {
                let native = plain_task(&fixture, "Native validation held");
                let task = fixture.runtime.get_task(&native).unwrap();
                let criteria = orbit_common::security::release::sha256_hex(
                    json!(task.acceptance_criteria).to_string().as_bytes(),
                );
                seed_pilot_receipt(
                    &fixture,
                    &native,
                    json!({
                        "assessment": {"task_id": native, "disposition": "selectors"},
                        "native_os_hold": {
                            "criteria": criteria,
                            "requirements": [{"criterion": 1, "os": "macos"}],
                        },
                    }),
                );
                assert_hold(&native, true);
                let response = operator.send(
                    "POST",
                    &pilot(&native),
                    json!({"verdict":verdict,"evidence":"Operator verified native evaluation."}),
                );
                assert_eq!(
                    response.status().as_u16(),
                    200,
                    "{}",
                    response.text().unwrap()
                );
                assert_hold(&native, false);
                let history = fixture.runtime.get_task_history(&native).unwrap();
                assert!(history.iter().any(|entry| {
                    entry.event == "native_os_requirement_resolved" && entry.by == "dashboard"
                }));
            }

            // Mark met: settles the owed criterion as the CLI comment does.
            let resp = operator.send(
                "POST",
                &deploy(&owed),
                json!({"criterion":2,"evidence":"Measured p95 121 ms over 20 samples."}),
            );
            assert_eq!(resp.status().as_u16(), 200, "{}", resp.text().unwrap());
            let history = fixture.runtime.get_task_history(&owed).unwrap();
            assert!(
                history
                    .iter()
                    .any(|e| e.event == "post_deploy_recorded" && e.by == "dashboard"),
                "history should show post_deploy_recorded by dashboard: {history:?}"
            );
        },
    );
}
