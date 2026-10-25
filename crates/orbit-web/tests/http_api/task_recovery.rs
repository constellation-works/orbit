use orbit_core::TaskStatus;
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::ExternalRef;
use serde_json::json;

use super::support::{Fixture, error_code, isolated};

fn blocked(fixture: &Fixture, title: &str) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            description: "recovery fixture".into(),
            status: Some(TaskStatus::Blocked),
            ..Default::default()
        })
        .unwrap()
        .id
}

#[test]
fn recovery_routes_gate_and_requeue_and_settle_refusal() {
    isolated(
        "task_recovery::recovery_routes_gate_and_requeue_and_settle_refusal",
        || {
            let fixture = Fixture::new();
            let resume_id = blocked(&fixture, "resume me");
            let discard_id = blocked(&fixture, "discard me");
            let settle_id = blocked(&fixture, "settle me");
            let ws = "?workspace=ws_http_fixture";
            let requeue = |id: &str| format!("/api/tasks/{id}/requeue{ws}");
            let settle = |id: &str| format!("/api/tasks/{id}/settle-merged{ws}");

            // Without operator session: same 403 as triage.
            let anon = fixture.server(false);
            error_code(
                anon.send("POST", &requeue(&resume_id), json!({"reason":"x"})),
                403,
                "authorization_denied",
            );
            error_code(
                anon.send("POST", &settle(&settle_id), json!({})),
                403,
                "authorization_denied",
            );
            assert_eq!(
                fixture.runtime.get_task(&resume_id).unwrap().status,
                TaskStatus::Blocked
            );

            let operator = fixture.server(true);
            // Missing reason.
            error_code(
                operator.send("POST", &requeue(&resume_id), json!({})),
                400,
                "requeue_reason_required",
            );
            // Resume keeping candidate (none recorded: next run fresh, still requeues).
            let resp = operator.send(
                "POST",
                &requeue(&resume_id),
                json!({"reason":"operator verified fix","discard_candidate":false}),
            );
            assert_eq!(resp.status().as_u16(), 200);
            let task = fixture.runtime.get_task(&resume_id).unwrap();
            assert_eq!(task.status, TaskStatus::Backlog);
            let history = fixture.runtime.get_task_history(&resume_id).unwrap();
            assert!(
                history.iter().any(|e| e.by == "dashboard"
                    && e.event == "operator_requeued"
                    && e.note
                        .as_deref()
                        .unwrap_or_default()
                        .contains("operator verified fix")),
                "history should name dashboard and reason: {history:?}"
            );
            // Discard path.
            let body: serde_json::Value = operator
                .send(
                    "POST",
                    &requeue(&discard_id),
                    json!({"reason":"start over","discard_candidate":true}),
                )
                .json()
                .unwrap();
            assert_eq!(body["candidate_discarded"], json!(true));
            assert_eq!(
                fixture.runtime.get_task(&discard_id).unwrap().status,
                TaskStatus::Backlog
            );
            let history = fixture.runtime.get_task_history(&discard_id).unwrap();
            assert!(history.iter().any(|e| e.by == "dashboard"));

            // Settle on unmerged/no-PR task: typed refusal, unchanged.
            let payload = error_code(
                operator.send("POST", &settle(&settle_id), json!({})),
                409,
                "settle_merged_refused",
            );
            assert!(
                payload["error"]
                    .as_str()
                    .unwrap()
                    .contains("does not settle")
            );
            assert_eq!(
                fixture.runtime.get_task(&settle_id).unwrap().status,
                TaskStatus::Blocked
            );
        },
    );
}

#[cfg(unix)]
#[test]
fn settle_merged_settles_only_when_pr_merged() {
    isolated(
        "task_recovery::settle_merged_settles_only_when_pr_merged",
        || {
            let fixture = Fixture::new();
            let mk = |title: &str, pr: u64| {
                fixture
                    .runtime
                    .add_task(TaskAddParams {
                        title: title.into(),
                        description: "settle fixture".into(),
                        status: Some(TaskStatus::Blocked),
                        external_refs: vec![ExternalRef::github_pr(pr.to_string()).unwrap()],
                        ..Default::default()
                    })
                    .unwrap()
                    .id
            };
            let merged_id = mk("merged", 42);
            let open_id = mk("open", 43);
            // Stub `gh`: PR 42 merged, PR 43 open, login orbit-bot.
            let bin = fixture.path("bin");
            std::fs::create_dir_all(&bin).unwrap();
            let script = format!(
                "#!/bin/sh\ncase \"$*\" in\n*\"api user\"*) echo orbit-bot ;;\n*\"pr view 42\"*) cat <<'JSON'\n{{\"number\":42,\"state\":\"MERGED\",\"headRefName\":\"orbit/{merged_id}-47642870\",\"baseRefName\":\"agent-main\",\"body\":\"Delivers {merged_id}.\",\"author\":{{\"login\":\"orbit-bot\"}},\"mergeCommit\":{{\"oid\":\"03f7af7d6a1b2c3d4e5f60718293a4b5c6d7e8f9\"}}}}\nJSON\n;;\n*\"pr view 43\"*) cat <<'JSON'\n{{\"number\":43,\"state\":\"OPEN\",\"headRefName\":\"orbit/{open_id}-47642870\",\"baseRefName\":\"agent-main\",\"body\":\"Delivers {open_id}.\",\"author\":{{\"login\":\"orbit-bot\"}}}}\nJSON\n;;\n*) echo '[]' ;;\nesac\n"
            );
            std::fs::write(bin.join("gh"), script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("gh"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let operator = fixture.server_with_path(true, &bin);
            let ws = "?workspace=ws_http_fixture";
            let settle = |id: &str| format!("/api/tasks/{id}/settle-merged{ws}");
            let payload = error_code(
                operator.send("POST", &settle(&open_id), json!({})),
                409,
                "settle_merged_refused",
            );
            assert!(
                payload["error"]
                    .as_str()
                    .unwrap()
                    .contains("does not settle")
            );
            assert_eq!(
                fixture.runtime.get_task(&open_id).unwrap().status,
                TaskStatus::Blocked
            );
            let body: serde_json::Value = operator
                .send("POST", &settle(&merged_id), json!({}))
                .json()
                .unwrap();
            assert_eq!(body["status"], json!("done"));
            assert_eq!(body["pr_number"], json!(42));
            assert_eq!(
                fixture.runtime.get_task(&merged_id).unwrap().status,
                TaskStatus::Done
            );
            let history = fixture.runtime.get_task_history(&merged_id).unwrap();
            assert!(
                history.iter().any(|e| e.by == "dashboard"),
                "settle history names dashboard: {history:?}"
            );
        },
    );
}
