use super::super::router;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use axum::response::Response;
use orbit_core::OrbitRuntime;
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::JobRunState;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

async fn request_dashboard_run_events(runtime: OrbitRuntime, encoded_run_id: &str) -> Response {
    request_dashboard_run_events_query(runtime, encoded_run_id, "").await
}

async fn request_dashboard_run_events_query(
    runtime: OrbitRuntime,
    encoded_run_id: &str,
    query: &str,
) -> Response {
    Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{encoded_run_id}/events{query}"))
                .header(header::HOST, "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

#[tokio::test]
async fn list_run_events_rejects_path_traversal_id() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request_dashboard_run_events(runtime, "..%2F..%2Fetc%2Fpasswd").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn force_cancel_exposes_stopped_and_unstopped_local_children() {
    use super::test_support::{body_json, enter_isolated_child, seed_run};
    use chrono::Utc;
    use orbit_types::workflow::{ChildDispatch, PipelineState};
    use serde_json::json;

    if !enter_isolated_child(
        module_path!(),
        "force_cancel_exposes_stopped_and_unstopped_local_children",
    ) {
        return;
    }
    let runtime = Arc::new(OrbitRuntime::in_memory().unwrap());
    let drain = seed_run(
        &runtime,
        "jrun-web-local-drain",
        "workspace_auto_pipeline",
        orbit_core::JobRunState::Pending,
    );
    let mut failed = seed_run(
        &runtime,
        "jrun-web-unstopped-child",
        "task_auto_pipeline",
        orbit_core::JobRunState::Running,
    );
    failed.pid = Some(std::process::id());
    runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&runtime.workspace_id().unwrap(), &failed, None)
        .unwrap();
    let stopped = seed_run(
        &runtime,
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
    let response = router()
        .with_state(crate::state::DashboardState::single(runtime.clone()))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/runs/{}/cancel", drain.run_id))
                .header(header::HOST, "localhost:7878")
                .header(header::ORIGIN, "http://localhost:7878")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"force":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
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
}

#[tokio::test]
async fn dashboard_cancel_requeues_with_reason_by_default_and_can_keep_blocked_state() {
    use super::test_support::{body_json, enter_isolated_child, seed_run};

    if !enter_isolated_child(
        module_path!(),
        "dashboard_cancel_requeues_with_reason_by_default_and_can_keep_blocked_state",
    ) {
        return;
    }
    for (run_id, block, expected_status) in [
        ("jrun-web-cancel-backlog", false, TaskStatus::Backlog),
        ("jrun-web-cancel-blocked", true, TaskStatus::Blocked),
    ] {
        let runtime = Arc::new(OrbitRuntime::in_memory().expect("build runtime"));
        let run = seed_run(&runtime, run_id, "task_pr_pipeline", JobRunState::Pending);
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
        let response = router()
            .with_state(crate::state::DashboardState::single(runtime.clone()))
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/runs/{}/cancel", run.run_id))
                    .header(header::HOST, "localhost:7878")
                    .header(header::ORIGIN, "http://localhost:7878")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let response = body_json(response).await;
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
        if !block {
            let history = runtime.get_task_history(&task.id).expect("task history");
            let entry = history.last().expect("cancellation history");
            assert_eq!(entry.event, "workflow_run_cancelled");
            assert!(
                entry
                    .note
                    .as_deref()
                    .is_some_and(|note| { note.contains("preserve this candidate for later") })
            );
        }
    }
}
