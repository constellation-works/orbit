use super::support::{Fixture, error_code, isolated};
use orbit_core::application::task::TaskAddParams;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, TaskStatus,
};
use serde_json::json;

#[test]
fn decision_listing_warns_when_a_bundle_has_a_missing_artifact() {
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::TaskArtifact;

    isolated(
        "decisions::decision_listing_warns_when_a_bundle_has_a_missing_artifact",
        || {
            let fixture = Fixture::new();
            let runtime = &fixture.runtime;
            let task = runtime
                .add_task(TaskAddParams {
                    title: "Task with missing evidence".into(),
                    ..Default::default()
                })
                .unwrap();
            runtime
                .update_task_with_identity(
                    &task.id,
                    TaskUpdateParams {
                        upsert_artifacts: vec![TaskArtifact::from_text(
                            "checks/owner-gates-after.log",
                            "passed",
                        )],
                        ..Default::default()
                    },
                    None,
                    Some("codex".into()),
                )
                .unwrap();
            let file = runtime
                .get_task_artifact_manifest(&task.id)
                .unwrap()
                .remove(0);
            let registry = TaskRegistryStore::open(&task_registry_path(&fixture.global)).unwrap();
            let bundle = registry
                .canonical_task_bundle_path(&runtime.workspace_id().unwrap(), &task.id)
                .unwrap();
            std::fs::remove_file(bundle.join("artifacts").join(file.blob)).unwrap();
            let server = fixture.server(true);
            let listed =
                super::support::json_ok(server.get("/api/decisions?workspace=ws_http_fixture"));
            assert_eq!(listed["total"], 0);
            assert!(listed["items"].as_array().unwrap().is_empty());
            assert!(
                listed["warnings"][0].as_str().unwrap().contains(&task.id),
                "{listed}"
            );
            assert!(
                !server
                    .get(&format!(
                        "/api/decisions?workspace=ws_http_fixture&task_id={}",
                        task.id
                    ))
                    .status()
                    .is_success()
            );
        },
    );
}

#[test]
fn decision_answer_refusals_keep_structured_client_errors() {
    isolated(
        "decisions::decision_answer_refusals_keep_structured_client_errors",
        || {
            let fixture = Fixture::new();
            let runtime = &fixture.runtime;
            let task = runtime
                .add_task(TaskAddParams {
                    title: "Blocked awaiting a decision".into(),
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                })
                .unwrap();
            let history = runtime.get_task_history(&task.id).unwrap();
            let blocked_at = history
                .iter()
                .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
                .unwrap()
                .at;
            let request = DecisionRequest::new(
                task.id.to_string(),
                DecisionGate::BlockTriage,
                vec![DecisionAnswer {
                    id: "dismiss".into(),
                    label: "Dismiss".into(),
                    consequence: "Leave blocked".into(),
                    option: None,
                }],
                DecisionBinding::BlockEpisode {
                    episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                },
                "block_triage".into(),
                chrono::Utc::now(),
            );
            runtime
                .apply_task_automation_update(
                    &task.id,
                    TaskAutomationUpdate {
                        append_history: vec![request.history_entry("system")],
                        ..Default::default()
                    },
                )
                .unwrap();
            let server = fixture.server(true);
            error_code(
                server.send(
                    "POST",
                    "/api/decisions/missing-request/answer",
                    json!({"answer": "approve"}),
                ),
                404,
                "request_not_found",
            );
            error_code(
                server.send(
                    "POST",
                    &format!("/api/decisions/{}/answer", request.id),
                    json!({"answer": "unknown"}),
                ),
                400,
                "answer_not_allowed",
            );
            assert_eq!(
                runtime.get_task(&task.id).unwrap().status,
                TaskStatus::Blocked
            );
        },
    );
}
