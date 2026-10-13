use super::support::{Fixture, isolated};
use axum::http::{StatusCode, header};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{AutoTaskAddParams, TaskStatus};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, JobRunState};
use serde_json::{Value, json};
use std::sync::Arc;
fn chore_params(name: &str) -> AutoTaskAddParams {
    AutoTaskAddParams {
        name: name.to_string(),
        description: format!("Definition {name}"),
        schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
        template: AutoTaskTemplate {
            title: format!("Chore {name}"),
            description: "Recurring chore body.".to_string(),
            acceptance_criteria: vec!["The chore is observable.".to_string()],
            task_type: TaskType::Chore,
            tags: vec![],
            required_tools: Vec::new(),
            context_files: Vec::new(),
            priority: TaskPriority::Medium,
            complexity: Some(TaskComplexity::Medium),
            crew: None,
            status: TaskStatus::Backlog,
        },
        dedupe: DedupePolicy::SkipIfOpen,
    }
}
#[test]
fn toggle_denies_a_caller_without_operator_capability() {
    isolated(
        "audit_contracts::toggle_denies_a_caller_without_operator_capability",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);
            fixture
                .runtime
                .auto_task_add(chore_params("nightly"))
                .expect("add");
            let response = server.send(
                "POST",
                "/api/auto-tasks/toggle?workspace=ws_http_fixture",
                json!({"name":"nightly","expected_enabled":true,"enabled":false}),
            );
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let json: Value = response.json().unwrap();
            assert_eq!(json["code"], "authorization_denied");
            assert_eq!(json["operation"], "auto_task.toggle");
            assert!(
                fixture
                    .runtime
                    .auto_task_show("nightly")
                    .unwrap()
                    .unwrap()
                    .enabled
            );
        },
    );
}

#[test]
fn require_localhost_origin_rejects_prefix_match() {
    isolated(
        "audit_contracts::require_localhost_origin_rejects_prefix_match",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            for (index, origin) in ["http://localhost.evil.com", "http://127.0.0.1.evil.com"]
                .into_iter()
                .enumerate()
            {
                let run = fixture.seed_run(
                    &format!("jrun-web-cancel-prefix-{index}"),
                    "web_cancel_prefix",
                    JobRunState::Pending,
                );
                let response = server
                    .request(
                        "POST",
                        &format!("/api/runs/{}/cancel?workspace=ws_http_fixture", run.run_id),
                    )
                    .header("origin", origin)
                    .json(&json!({}))
                    .send()
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
                assert_eq!(
                    fixture.runtime.show_job_run(&run.run_id).unwrap().state,
                    JobRunState::Pending
                );
            }
        },
    );
}

#[test]
fn require_localhost_origin_rejects_forged_host_get_without_origin() {
    isolated(
        "audit_contracts::require_localhost_origin_rejects_forged_host_get_without_origin",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let response = server
                .request("GET", "/api/tasks?workspace=ws_http_fixture")
                .header("host", "attacker.example:7878")
                .send()
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert_eq!(
                response.json::<Value>().unwrap()["error"],
                "cross-origin requests not allowed"
            );
        },
    );
}

#[test]
fn list_run_events_rejects_path_traversal_id() {
    isolated(
        "audit_contracts::list_run_events_rejects_path_traversal_id",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let response =
                server.get("/api/runs/..%2F..%2Fetc%2Fpasswd/events?workspace=ws_http_fixture");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        },
    );
}

#[test]
fn dashboard_cancel_requeues_with_reason_by_default_and_can_keep_blocked_state() {
    isolated(
        "audit_contracts::dashboard_cancel_requeues_with_reason_by_default_and_can_keep_blocked_state",
        || {
            let fixture = Fixture::new();
            std::fs::create_dir_all(fixture.runtime.paths().repo_root.join("src")).unwrap();
            std::fs::write(
                fixture.runtime.paths().repo_root.join("src/candidate.rs"),
                "",
            )
            .unwrap();
            let server = fixture.server(true);

            for (run_id, block, expected_status) in [
                ("jrun-web-cancel-backlog", false, TaskStatus::Backlog),
                ("jrun-web-cancel-blocked", true, TaskStatus::Blocked),
            ] {
                let runtime = Arc::new(fixture.runtime.clone());
                let run = fixture.seed_run(run_id, "task_pr_pipeline", JobRunState::Pending);
                let task = runtime
                    .add_task(TaskAddParams {
                        title: "dashboard cancelled task".into(),
                        description: "candidate should remain available".into(),
                        plan: "Resume the candidate after cancellation".into(),
                        context_files: vec!["file:src/candidate.rs".into()],
                        status: Some(TaskStatus::Backlog),
                        priority: TaskPriority::Medium,
                        complexity: TaskComplexity::Low,
                        task_type: Some(TaskType::Bug),
                        ..TaskAddParams::default()
                    })
                    .expect("create task");
                runtime
                    .update_task_as_human(
                        &task.id,
                        TaskUpdateParams {
                            status: Some(TaskStatus::InProgress),
                            job_run_id: Some(Some(run.run_id.clone())),
                            ..TaskUpdateParams::default()
                        },
                        "test operator".into(),
                    )
                    .expect("couple task to run");

                let body = if block {
                    json!({
                        "reason": "preserve this candidate for later",
                        "block": true,
                    })
                } else {
                    json!({"reason": "preserve this candidate for later"})
                };
                let response = server.send(
                    "POST",
                    &format!("/api/runs/{}/cancel?workspace=ws_http_fixture", run.run_id),
                    body,
                );
                assert_eq!(response.status(), StatusCode::OK);
                let response = response.json::<Value>().unwrap();
                assert_eq!(response["outcome"], "cancelled");
                let task_after_cancel = runtime.get_task(&task.id).expect("task after cancel");
                assert_eq!(task_after_cancel.status, expected_status);
                assert_eq!(
                    task_after_cancel.plan,
                    "Resume the candidate after cancellation"
                );
                assert_eq!(
                    task_after_cancel.context_files,
                    vec!["file:src/candidate.rs".to_string()]
                );
                let policy = runtime
                    .read_run_state(&run.run_id)
                    .expect("run state")
                    .and_then(|state| state.task_cancellation_policy)
                    .expect("durable cancellation policy");
                assert_eq!(policy.block, block);
                let history = runtime.get_task_history(&task.id).expect("task history");
                let entry = history.last().expect("cancellation history");
                assert_eq!(entry.from_status, Some(TaskStatus::InProgress));
                assert_eq!(entry.to_status, Some(expected_status));
                assert_eq!(
                    entry.event,
                    if block {
                        "workflow_run_failed"
                    } else {
                        "workflow_run_cancelled"
                    }
                );
            }
        },
    );
}

#[test]
fn api_and_static_responses_carry_baseline_security_headers() {
    isolated(
        "audit_contracts::api_and_static_responses_carry_baseline_security_headers",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            for uri in [
                "/api/tasks?workspace=ws_http_fixture",
                "/static/dashboard.css",
                "/no-such-route",
            ] {
                let response = server.get(uri);
                let headers = response.headers();
                assert_eq!(headers["x-content-type-options"], "nosniff", "{uri}");
                assert_eq!(headers["referrer-policy"], "no-referrer", "{uri}");
                assert_eq!(
                    headers["cross-origin-resource-policy"], "same-origin",
                    "{uri}"
                );
            }
            assert!(
                server
                    .get("/static/dashboard.css")
                    .headers()
                    .contains_key("content-security-policy"),
                "static assets keep their CSP"
            );
        },
    );
}

#[test]
fn dashboard_routes_emit_csp() {
    isolated("audit_contracts::dashboard_routes_emit_csp", || {
        let fixture = Fixture::new();
        let server = fixture.server(true);
        let assets = include_str!("../../src/assets.rs");
        let mut routes = vec!["/"];
        for line in assets.lines() {
            let line = line.trim();
            if let Some(route) = line
                .strip_prefix('"')
                .and_then(|s| s.split('"').next())
                .filter(|s| s.starts_with("/static/"))
            {
                routes.push(route);
            }
        }
        // CSS's inline tuple is the only registration not on its own line.
        routes.push("/static/dashboard.css");
        let expected = server.get("/").headers()[header::CONTENT_SECURITY_POLICY].clone();
        for route in routes {
            let response = server.get(route);
            assert_eq!(response.status(), StatusCode::OK, "{route}");
            assert_eq!(
                response.headers().get(header::CONTENT_SECURITY_POLICY),
                Some(&expected),
                "{route} route must emit the dashboard CSP"
            );
        }
    });
}

#[test]
fn force_cancel_exposes_stopped_and_unstopped_local_children() {
    isolated(
        "audit_contracts::force_cancel_exposes_stopped_and_unstopped_local_children",
        || {
            use chrono::Utc;
            use orbit_types::workflow::{ChildDispatch, PipelineState};
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let runtime = Arc::new(fixture.runtime.clone());
            let drain = fixture.seed_run(
                "jrun-web-local-drain",
                "workspace_auto_pipeline",
                orbit_core::JobRunState::Pending,
            );
            let mut failed = fixture.seed_run(
                "jrun-web-unstopped-child",
                "task_auto_pipeline",
                orbit_core::JobRunState::Running,
            );
            failed.pid = Some(server.pid());
            runtime
                .sqlite_store()
                .unwrap()
                .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &failed, None)
                .unwrap();
            let stopped = fixture.seed_run(
                "jrun-web-stopped-child",
                "task_auto_pipeline",
                orbit_core::JobRunState::Pending,
            );
            let mut state = PipelineState::new(drain.run_id.clone(), drain.job_id, json!({}));
            for child in [&failed.run_id, &stopped.run_id] {
                state.record_child_dispatch(ChildDispatch::submitted(
                    child.clone(),
                    "task_auto_pipeline".into(),
                    "dispatch".into(),
                    false,
                    true,
                    Utc::now(),
                ));
            }
            runtime.write_run_state(&drain.run_id, &state).unwrap();
            let response = server.send(
                "POST",
                &format!(
                    "/api/runs/{}/cancel?workspace=ws_http_fixture",
                    drain.run_id
                ),
                json!({"force":true}),
            );
            assert_eq!(response.status(), StatusCode::OK);
            let body = response.json::<Value>().unwrap();
            assert_eq!(body["outcome"], "cancelled");
            assert_eq!(body["final_state"], "cancelled");
            assert_eq!(body["forced_runs"], json!([stopped.run_id]));
            assert_eq!(body["unstopped_children"][0]["child_run_id"], failed.run_id);
            assert!(
                body["unstopped_children"][0]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("could not confirm")
            );
            assert_eq!(
                runtime
                    .show_job_run(&failed.run_id)
                    .unwrap()
                    .state
                    .to_string(),
                "running"
            );
        },
    );
}
