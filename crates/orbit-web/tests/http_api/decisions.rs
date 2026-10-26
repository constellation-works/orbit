use super::support::{Fixture, error_code, isolated};
use orbit_core::application::task::TaskAddParams;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::{
    DecisionAnswer, DecisionBinding, DecisionGate, DecisionRequest, TaskStatus,
};
use serde_json::json;

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
