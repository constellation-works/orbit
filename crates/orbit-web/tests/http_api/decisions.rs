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
                    risk_tier: Default::default(),
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

/// A blocked task holding one open block-triage request, as block triage
/// raises it, and that request.
fn blocked_task_with_request(runtime: &orbit_core::OrbitRuntime, title: &str) -> DecisionRequest {
    let task = runtime
        .add_task(TaskAddParams {
            title: title.into(),
            status: Some(TaskStatus::Blocked),
            ..Default::default()
        })
        .unwrap();
    raise_block_triage(runtime, &task.id)
}

fn raise_block_triage(runtime: &orbit_core::OrbitRuntime, task_id: &str) -> DecisionRequest {
    let blocked_at = runtime
        .get_task_history(task_id)
        .unwrap()
        .iter()
        .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
        .unwrap()
        .at;
    let request = DecisionRequest::new(
        task_id.to_string(),
        DecisionGate::BlockTriage,
        vec![DecisionAnswer {
            risk_tier: Default::default(),
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
            task_id,
            TaskAutomationUpdate {
                append_history: vec![request.history_entry("system")],
                ..Default::default()
            },
        )
        .unwrap();
    request
}

fn listed_ids(listed: &serde_json::Value) -> Vec<String> {
    listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_string())
        .collect()
}

/// [ORB-21835] The dashboard's waiting count lists open decisions every 15
/// seconds; on a ~10k-task workspace each list read every task bundle and
/// took seconds although no task held a request. A later read now skips a
/// bundle the store has not written since a clean read found no request in
/// it, and still reads every changed bundle: new, answered and re-raised
/// requests and fresh corruption all show on the next read.
/// [ORB-22913] Warm scans must also avoid hydrating every task's envelope
/// merely to select ids; terminal bundles still participate in corruption
/// detection and closed-request listings.
#[cfg(unix)]
#[test]
fn decision_listing_rereads_only_bundles_changed_since_a_clean_read() {
    use orbit_core::application::task::TaskUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::TaskArtifact;
    use std::os::unix::fs::PermissionsExt;

    isolated(
        "decisions::decision_listing_rereads_only_bundles_changed_since_a_clean_read",
        || {
            let fixture = Fixture::new();
            let runtime = &fixture.runtime;
            let waiting = blocked_task_with_request(runtime, "Waits on a decision");
            let quiet = runtime
                .add_task(TaskAddParams {
                    title: "Blocked, nothing asked yet".into(),
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                })
                .unwrap();
            let evidence = runtime
                .add_task(TaskAddParams {
                    title: "Holds evidence".into(),
                    status: Some(TaskStatus::Done),
                    ..Default::default()
                })
                .unwrap();
            runtime
                .update_task_with_identity(
                    &evidence.id,
                    TaskUpdateParams {
                        upsert_artifacts: vec![TaskArtifact::from_text(
                            "checks/gates.log",
                            "passed",
                        )],
                        ..Default::default()
                    },
                    None,
                    Some("codex".into()),
                )
                .unwrap();
            let registry = TaskRegistryStore::open(&task_registry_path(&fixture.global)).unwrap();
            let workspace = runtime.workspace_id().unwrap();
            let bundle = |id: &str| registry.canonical_task_bundle_path(&workspace, id).unwrap();
            let server = fixture.counted_task_server();
            let list = |query: &str| {
                super::support::json_ok(
                    server.get(&format!("/api/decisions?workspace=ws_http_fixture{query}")),
                )
            };

            let first = list("");
            assert_eq!(listed_ids(&first), vec![waiting.id.clone()], "{first}");
            assert!(first.get("warnings").is_none(), "{first}");

            // Make the quiet task's bundle unreadable without a store write:
            // a permission change moves no stamp. A read of that bundle now
            // fails, so a clean answer shows the list did not read it.
            let description = bundle(&quiet.id).join("description.md");
            std::fs::set_permissions(&description, std::fs::Permissions::from_mode(0o000)).unwrap();
            let trace_before = server.task_query_trace().len();
            let unread = list("");
            std::fs::set_permissions(&description, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                listed_ids(&unread),
                vec![waiting.id.clone()],
                "a bundle unchanged since its clean read must not be read again: {unread}"
            );
            assert!(unread.get("warnings").is_none(), "{unread}");
            let trace = server.task_query_trace();
            assert_eq!(
                trace[trace_before..]
                    .iter()
                    .filter(|row| row["span"]["name"] == "task_index_freshness")
                    .count(),
                0,
                "ORB-22913: whole-bundle proofs must avoid a redundant workspace envelope freshness pass"
            );
            assert_eq!(
                trace[trace_before..]
                    .iter()
                    .filter(|row| row["span"]["name"] == "task_envelope_selection")
                    .count(),
                0,
                "ORB-22913: polling the waiting count must select ids without hydrating workspace envelopes"
            );

            // A request raised on the skipped task shows on the next read.
            let raised = raise_block_triage(runtime, &quiet.id);
            let both = list("");
            assert_eq!(
                listed_ids(&both),
                vec![waiting.id.clone(), raised.id.clone()],
                "{both}"
            );

            // An answer closes the request on the next read; closed requests
            // still list on request, with their effective state.
            super::support::json_ok(server.send(
                "POST",
                &format!(
                    "/api/decisions/{}/answer?workspace=ws_http_fixture",
                    waiting.id
                ),
                json!({"answer": "dismiss"}),
            ));
            let open = list("");
            assert_eq!(listed_ids(&open), vec![raised.id.clone()], "{open}");
            let closed = list("&include_closed=true");
            assert_eq!(
                listed_ids(&closed),
                vec![waiting.id.clone(), raised.id.clone()],
                "{closed}"
            );
            assert_eq!(closed["items"][0]["state"], "answered", "{closed}");
            assert_eq!(closed["items"][1]["state"], "open", "{closed}");
            assert_eq!(list("&gate=block_triage"), open);
            assert!(listed_ids(&list("&gate=script_grant&include_closed=true")).is_empty());
            let task_query = format!("&task_id={}", waiting.task_id);
            assert!(listed_ids(&list(&task_query)).is_empty());
            let task_closed = list(&format!(
                "{task_query}&include_closed=true&gate=block_triage"
            ));
            assert_eq!(listed_ids(&task_closed), vec![waiting.id.clone()]);
            assert_eq!(task_closed["items"][0], closed["items"][0]);

            // Damage to a bundle read clean before still warns.
            let file = runtime
                .get_task_artifact_manifest(&evidence.id)
                .unwrap()
                .remove(0);
            std::fs::remove_file(bundle(&evidence.id).join("artifacts").join(file.blob)).unwrap();
            let damaged = list("");
            assert_eq!(listed_ids(&damaged), vec![raised.id.clone()], "{damaged}");
            assert!(
                damaged["warnings"][0]
                    .as_str()
                    .unwrap()
                    .contains(evidence.id.as_str()),
                "{damaged}"
            );
            assert_eq!(list("&gate=script_grant")["warnings"], damaged["warnings"]);
            assert_eq!(
                list("&include_closed=true")["warnings"],
                damaged["warnings"]
            );
            assert!(
                !server
                    .get(&format!(
                        "/api/decisions?workspace=ws_http_fixture&task_id={}",
                        evidence.id
                    ))
                    .status()
                    .is_success()
            );
        },
    );
}
