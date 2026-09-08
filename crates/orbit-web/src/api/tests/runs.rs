//! Test-only allowlist: the original tests under orbit-cli passed the same lints via
//! the crate-level test harness configuration; duplicated here for the extracted crate.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use chrono::Utc;
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{JobRun, JobRunState, OrbitRuntime, TaskStatus, V2AuditEventInsertParams};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::super::router;
use super::super::runs::*;
use super::test_support::{body_json, seed_run, write_replay_job, write_seeded_run};

async fn request_cancel(runtime: OrbitRuntime, run_id: &str, origin: Option<&str>) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/runs/{run_id}/cancel"));
    if let Some(origin) = origin {
        builder = builder
            .header(header::ORIGIN, origin)
            .header(header::HOST, "localhost:3000");
    }
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

async fn request_resume(runtime: OrbitRuntime, run_id: &str, origin: Option<&str>) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri(format!("/job-runs/{run_id}/resume"));
    if let Some(origin) = origin {
        builder = builder
            .header(header::ORIGIN, origin)
            .header(header::HOST, "localhost:3000");
    }
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(Body::empty()).expect("request"))
        .await
        .expect("response")
}

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
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

async fn request_dashboard_run_logs(runtime: OrbitRuntime, encoded_run_id: &str) -> Response {
    Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri(format!("/api/runs/{encoded_run_id}/logs"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

fn seed_cli_invocation_audit(runtime: &OrbitRuntime, run_id: &str, stderr: &[u8]) -> String {
    let audit_root = runtime.data_root().join("state").join("audit");
    let blob_store = BlobStore::new(audit_root.join("blobs"));
    let stdout_ref = blob_store
        .write(b"normal output\n")
        .expect("write stdout blob");
    let stderr_ref = blob_store.write(stderr).expect("write stderr blob");
    seed_v2_audit_events(
        runtime,
        run_id,
        vec![
            json!({
                "schemaVersion": 1,
                "event_type": "run.started",
                "event_id": "evt-run",
                "ts": "2026-05-08T04:12:20Z",
                "run_id": run_id,
                "body_kind": "run_started"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": "evt-step",
                "ts": "2026-05-08T04:12:21Z",
                "run_id": run_id,
                "parent_event_id": "evt-run",
                "body_kind": "step_started",
                "step_id": "implement"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "cli.invocation.finished",
                "event_id": "evt-cli",
                "ts": "2026-05-08T04:12:22Z",
                "run_id": run_id,
                "parent_event_id": "evt-step",
                "body_kind": "cli_invocation_finished",
                "provider": "codex",
                "stdout_blob_ref": stdout_ref,
                "stderr_blob_ref": stderr_ref,
                "exit_code": 0,
                "timed_out": false,
                "duration_ms": 123
            }),
        ],
    );
    stderr_ref
}

fn seed_v2_audit_events(
    runtime: &OrbitRuntime,
    run_id: &str,
    events: impl IntoIterator<Item = Value>,
) {
    let workspace_id = runtime.workspace_id().expect("workspace id");
    for (index, mut event) in events.into_iter().enumerate() {
        let object = event.as_object_mut().expect("event object");
        object
            .entry("schemaVersion".to_string())
            .or_insert_with(|| json!(1));
        object
            .entry("run_id".to_string())
            .or_insert_with(|| json!(run_id));
        object
            .entry("agent_identity".to_string())
            .or_insert_with(|| json!("system"));
        object.entry("ts".to_string()).or_insert_with(|| {
            json!(format!(
                "2026-05-08T04:{:02}:{:02}Z",
                (index / 60) % 60,
                index % 60
            ))
        });

        let ts = event
            .get("ts")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            .expect("event ts");
        runtime
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: workspace_id.clone(),
                event_id: event["event_id"].as_str().expect("event id").to_string(),
                source: "v2_envelope".to_string(),
                schema_version: event["schemaVersion"]
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(1),
                event_type: event["event_type"]
                    .as_str()
                    .expect("event type")
                    .to_string(),
                ts,
                run_id: event["run_id"].as_str().expect("run id").to_string(),
                agent_identity: event["agent_identity"]
                    .as_str()
                    .expect("agent identity")
                    .to_string(),
                parent_event_id: event
                    .get("parent_event_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                workspace_path: None,
                payload_json: event.to_string(),
            })
            .expect("insert v2 audit event");
    }
}

#[tokio::test]
async fn list_run_logs_returns_bounded_redacted_step_records() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-log-api";
    let mut stderr = String::from("first line\n");
    stderr.push_str("Authorization: Bearer sk-test-secret\n");
    for index in 0..200 {
        stderr.push_str(&format!("line {index}\n"));
    }
    let stderr_ref = seed_cli_invocation_audit(&runtime, run_id, stderr.as_bytes());

    let response = request_dashboard_run_logs(runtime, run_id).await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    let rows = payload.as_array().expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["run_id"], run_id);
    assert_eq!(rows[0]["event_id"], "evt-cli");
    assert_eq!(rows[0]["step_id"], "implement");
    assert_eq!(rows[0]["step_index"], 0);
    assert_eq!(rows[0]["provider"], "codex");
    assert_eq!(rows[0]["stderr_blob_ref"], stderr_ref);
    assert_eq!(rows[0]["exit_code"], 0);
    assert_eq!(rows[0]["timed_out"], false);
    assert_eq!(rows[0]["duration_ms"], 123);
    let preview = rows[0]["stderr_preview"].as_str().expect("stderr preview");
    assert!(preview.contains("[REDACTED_AUTH]"));
    assert!(!preview.contains("sk-test-secret"));
    assert_eq!(rows[0]["stderr_truncated"], true);
}

#[tokio::test]
async fn list_run_events_rejects_path_traversal_id() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request_dashboard_run_events(runtime, "..%2F..%2Fetc%2Fpasswd").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_run_events_rejects_id_with_slashes() {
    let cases = [
        ("jrun%2F1", "literal slash"),
        ("jrun%5C1", "backslash"),
        (".jrun-1", "leading dot"),
        ("jrun%00nul", "nul byte"),
    ];

    for (encoded_run_id, label) in cases {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");

        let response = request_dashboard_run_events(runtime, encoded_run_id).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{label}");
    }
}

#[tokio::test]
async fn list_run_events_streams_small_page_from_oversized_fixture() {
    // The persisted audit table can contain many rows for a run. The endpoint
    // must fill a small page from the head of the result set without scanning
    // every event first.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-events-oversize";
    seed_v2_audit_events(
        &runtime,
        run_id,
        (0..10usize).map(|index| {
            json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": format!("evt-{index}"),
                "run_id": run_id,
                "body_kind": "step_started"
            })
        }),
    );

    let response = request_dashboard_run_events_query(runtime, run_id, "?limit=5").await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    let events = payload.as_array().expect("events array");
    assert_eq!(events.len(), 5);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["event_id"], format!("evt-{index}"));
        assert_eq!(event["body_kind"], "step_started");
    }
}

#[tokio::test]
async fn list_run_events_returns_payload_too_large_when_scan_budget_exceeded() {
    // Fill the table with valid events whose `body_kind` does NOT match the
    // requested filter, forcing the endpoint to walk past the row budget
    // without ever filling the page.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-events-budget";
    seed_v2_audit_events(
        &runtime,
        run_id,
        (0..=super::super::runs::RUN_EVENTS_MAX_SCAN_LINES).map(|index| {
            json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": format!("evt-budget-{index}"),
                "run_id": run_id,
                "body_kind": "step_started"
            })
        }),
    );

    let response =
        request_dashboard_run_events_query(runtime, run_id, "?kind=does_not_match").await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let payload = body_json(response).await;
    let error = payload["error"].as_str().expect("error message");
    assert!(
        error.contains("bounded scan budget"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn list_run_events_kind_filter_still_works_with_streaming() {
    // Confirms AC3: streaming preserves kind filtering correctness.
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-events-kind";
    seed_v2_audit_events(
        &runtime,
        run_id,
        vec![
            json!({
                "schemaVersion": 1,
                "event_type": "run.started",
                "event_id": "evt-run",
                "run_id": run_id,
                "body_kind": "run_started"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": "evt-step-a",
                "run_id": run_id,
                "body_kind": "step_started"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": "evt-step-b",
                "run_id": run_id,
                "body_kind": "step_started"
            }),
        ],
    );

    let response = request_dashboard_run_events_query(runtime, run_id, "?kind=step_started").await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    let events = payload.as_array().expect("events array");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["event_id"], "evt-step-a");
    assert_eq!(events[1]["event_id"], "evt-step-b");
}

#[tokio::test]
async fn list_run_events_accepts_valid_run_id() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-1";
    seed_v2_audit_events(
        &runtime,
        run_id,
        vec![json!({
            "schemaVersion": 1,
            "event_type": "step.started",
            "event_id": "evt-step-started",
            "run_id": run_id,
            "body_kind": "step_started"
        })],
    );

    let response = request_dashboard_run_events(runtime, run_id).await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    let events = payload.as_array().expect("events array");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["run_id"], run_id);
    assert_eq!(events[0]["body_kind"], "step_started");
}

#[tokio::test]
async fn cancel_run_endpoint_cancels_pending_run() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = seed_run(
        &runtime,
        "jrun-web-cancel-pending",
        "web_cancel_pending",
        JobRunState::Pending,
    );

    let response =
        request_cancel(runtime.clone(), &run.run_id, Some("http://localhost:3000")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["run_id"], run.run_id);
    assert_eq!(payload["outcome"], "cancelled");
    assert_eq!(payload["previous_state"], "pending");
    assert_eq!(payload["final_state"], "cancelled");
    assert_eq!(payload["signal_attempted"], false);
    assert_eq!(payload["signal_outcome"], Value::Null);
    let stored = runtime.show_job_run(&run.run_id).expect("show cancelled");
    assert_eq!(stored.state, JobRunState::Cancelled);
}

#[tokio::test]
async fn cancel_run_endpoint_reports_terminal_run_idempotently_without_mutating_bundle() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = seed_run(
        &runtime,
        "jrun-web-cancel-terminal",
        "web_cancel_terminal",
        JobRunState::Success,
    );
    let before = runtime.show_job_run(&run.run_id).expect("show before");

    let response =
        request_cancel(runtime.clone(), &run.run_id, Some("http://localhost:3000")).await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    assert_eq!(payload["outcome"], "already_terminal");
    assert_eq!(payload["previous_state"], "success");
    assert_eq!(payload["final_state"], "success");
    assert_eq!(payload["signal_attempted"], false);
    let after = runtime.show_job_run(&run.run_id).expect("show after");
    assert_eq!(after, before);
}

#[tokio::test]
async fn cancel_run_endpoint_applies_localhost_origin_guard() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run = seed_run(
        &runtime,
        "jrun-web-cancel-origin",
        "web_cancel_origin",
        JobRunState::Pending,
    );

    let response = request_cancel(runtime.clone(), &run.run_id, Some("https://example.test")).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let stored = runtime.show_job_run(&run.run_id).expect("show run");
    assert_eq!(stored.state, JobRunState::Pending);
}

#[tokio::test]
async fn resume_job_run_endpoint_rejects_non_terminal_run_with_guard_reason() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let source = seed_run(
        &runtime,
        "jrun-web-resume-pending",
        "web_resume_pending",
        JobRunState::Pending,
    );

    let response = request_resume(runtime, &source.run_id, Some("http://localhost:3000")).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let payload = body_json(response).await;
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("is pending"))
    );
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("interrupted, failed, or timed-out"))
    );
}

#[tokio::test]
async fn resume_job_run_endpoint_returns_not_found_for_unknown_run() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request_resume(
        runtime,
        "jrun-web-resume-missing",
        Some("http://localhost:3000"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let payload = body_json(response).await;
    assert_eq!(payload["error"], "run not found: jrun-web-resume-missing");
}

#[test]
fn run_detail_uses_v2_audit_steps_when_step_bundle_is_empty() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-web-audit-step";
    seed_v2_audit_events(
        &runtime,
        run_id,
        vec![
            json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": "evt-step-started",
                "ts": "2026-04-28T00:00:01Z",
                "run_id": run_id,
                "agent_identity": "system",
                "body_kind": "step_started",
                "step_id": "build"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "step.finished",
                "event_id": "evt-step-finished",
                "ts": "2026-04-28T00:00:03Z",
                "run_id": run_id,
                "agent_identity": "system",
                "body_kind": "step_finished",
                "step_id": "build",
                "outcome": "success"
            }),
        ],
    );
    let scheduled_at = chrono::DateTime::parse_from_rfc3339("2026-04-28T00:00:00Z")
        .expect("parse scheduled")
        .with_timezone(&Utc);
    let run = orbit_core::JobRun {
        run_id: run_id.to_string(),
        job_id: "job-web".to_string(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at,
        started_at: Some(scheduled_at),
        finished_at: Some(scheduled_at),
        duration_ms: Some(2_000),
        created_at: scheduled_at,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };

    let detail = job_run_detail_to_json(&runtime, &run);
    let steps = detail["steps"].as_array().expect("steps array");

    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["step_index"], 0);
    assert_eq!(steps[0]["target_type"], "activity");
    assert_eq!(steps[0]["target_id"], "build");
    assert_eq!(steps[0]["state"], "success");
    assert_eq!(steps[0]["duration_ms"], 2_000);
}

#[test]
fn run_detail_keeps_a_parallel_provider_open_when_another_invocation_finishes() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-web-provider-pid";
    seed_v2_audit_events(
        &runtime,
        run_id,
        vec![
            json!({
                "schemaVersion": 1,
                "event_type": "step.started",
                "event_id": "evt-step-started",
                "ts": "2026-07-27T02:41:01Z",
                "run_id": run_id,
                "agent_identity": "codex",
                "body_kind": "step_started",
                "step_id": "agent_implement"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "activity.started",
                "event_id": "evt-live-invocation",
                "ts": "2026-07-27T02:41:02Z",
                "run_id": run_id,
                "agent_identity": "codex",
                "parent_event_id": "evt-step-started",
                "body_kind": "activity_started"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "activity.started",
                "event_id": "evt-finished-invocation",
                "ts": "2026-07-27T02:41:03Z",
                "run_id": run_id,
                "agent_identity": "codex",
                "parent_event_id": "evt-step-started",
                "body_kind": "activity_started"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "cli.invocation.process",
                "event_id": "evt-finished-pid",
                "ts": "2026-07-27T02:41:04Z",
                "run_id": run_id,
                "agent_identity": "codex",
                "parent_event_id": "evt-finished-invocation",
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                "pid": u32::MAX - 2
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "cli.invocation.process",
                "event_id": "evt-live-pid",
                "ts": "2026-07-27T02:41:05Z",
                "run_id": run_id,
                "agent_identity": "codex",
                "parent_event_id": "evt-live-invocation",
                "body_kind": "cli_invocation_process",
                "provider": "codex",
                // This PID is unreachable: the projection must retain the
                // open record and report its liveness independently.
                "pid": u32::MAX - 1,
                "pid_start_time": "ps-lstart-utc-v1:seeded"
            }),
            json!({
                "schemaVersion": 1,
                "event_type": "cli.invocation.finished",
                "event_id": "evt-finished",
                "ts": "2026-07-27T02:41:06Z",
                "run_id": run_id,
                "agent_identity": "codex",
                "parent_event_id": "evt-finished-invocation",
                "body_kind": "cli_invocation_finished",
                "exit_code": 0
            }),
        ],
    );
    let scheduled_at = chrono::DateTime::parse_from_rfc3339("2026-07-27T02:41:00Z")
        .expect("parse scheduled")
        .with_timezone(&Utc);
    let run = orbit_core::JobRun {
        run_id: run_id.to_string(),
        job_id: "task_pr_pipeline".to_string(),
        attempt: 1,
        state: JobRunState::Running,
        scheduled_at,
        started_at: Some(scheduled_at),
        finished_at: None,
        duration_ms: None,
        created_at: scheduled_at,
        pid: None,
        pid_start_time: None,
        input: None,
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };

    let detail = job_run_detail_to_json(&runtime, &run);
    let processes = detail["provider_processes"]
        .as_array()
        .expect("provider_processes array");

    assert_eq!(processes.len(), 2);
    assert_eq!(processes[0]["pid"], u32::MAX - 2);
    assert_eq!(processes[0]["finished"], true);
    assert_eq!(processes[0]["exit_code"], 0);
    assert_eq!(processes[1]["pid"], u32::MAX - 1);
    assert_eq!(processes[1]["provider"], "codex");
    assert_eq!(processes[1]["step_id"], "agent_implement");
    assert_eq!(processes[1]["finished"], false);
    assert_eq!(processes[1]["exit_code"], Value::Null);
    if cfg!(unix) {
        assert_eq!(processes[1]["liveness"], "exited");
    } else {
        assert_eq!(processes[1]["liveness"], "unknown");
    }
}

async fn request_ship(runtime: OrbitRuntime, body: Option<Value>) -> Response {
    let mut builder = Request::builder()
        .method(Method::POST)
        .uri("/workflows/ship")
        .header(header::ORIGIN, "http://localhost:3000")
        .header(header::HOST, "localhost:3000");
    let body = match body {
        Some(json) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(builder.body(body).expect("request"))
        .await
        .expect("response")
}

#[tokio::test]
async fn ship_endpoint_rejects_unknown_mode() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request_ship(runtime, Some(json!({ "mode": "yolo" }))).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = body_json(response).await;
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("unknown ship mode"))
    );
}

async fn request_ship_global(
    state: crate::state::DashboardState,
    uri: &str,
    body: Value,
) -> Response {
    router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::ORIGIN, "http://localhost:7878")
                .header(header::HOST, "localhost:7878")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .expect("request"),
        )
        .await
        .expect("response")
}

/// ORB-10008: an unknown `?workspace=` on the ship endpoint is a clean 404
/// JSON rejection from the workspace extractor, not a 500.
#[tokio::test]
async fn ship_endpoint_rejects_unknown_workspace_with_404_json() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let state = crate::state::DashboardState::single(Arc::new(runtime));

    let response = request_ship_global(
        state,
        "/workflows/ship?workspace=ghost",
        json!({ "mode": "pr" }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let payload = body_json(response).await;
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|m| m.contains("unknown workspace: ghost"))
    );
}

/// ORB-10444: Ship is a write against a live pipeline, so a second click while
/// the task already has a run in flight must not create a duplicate run. The
/// in-flight run is seeded as `pending` with no owner pid, which the run-owner
/// reconciler leaves alone inside its unclaimed grace window — so the guard,
/// not a reconciliation race, is what this asserts.
///
/// Explicit task ids are validated before the guard (shared `submit_ship_run`
/// path), so the fixture seeds a real backlog task rather than a synthetic id.
/// Refusal still happens before job submit, so no pipeline worker is spawned.
#[tokio::test]
async fn ship_endpoint_refuses_second_dispatch_while_task_run_is_in_flight() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    write_replay_job(&runtime, "task_auto_pipeline");
    let task_id = runtime
        .add_task(TaskAddParams {
            title: "in-flight ship guard fixture".to_string(),
            description: "seeded so explicit ship selection validates".to_string(),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed ship fixture task")
        .id;
    let mut in_flight = seed_run(
        &runtime,
        "jrun-in-flight",
        "task_auto_pipeline",
        JobRunState::Pending,
    );
    in_flight.input = Some(json!({ "mode": "local", "task_ids": [task_id] }));
    write_seeded_run(&runtime, &in_flight);

    let response = request_ship(
        runtime.clone(),
        Some(json!({ "task_ids": [task_id], "mode": "local" })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let payload = body_json(response).await;
    assert_eq!(payload["code"].as_str(), Some("ship_run_in_flight"));
    assert_eq!(payload["run_id"].as_str(), Some("jrun-in-flight"));
    assert_eq!(payload["task_id"].as_str(), Some(task_id.as_str()));

    let runs = runtime
        .list_job_runs(JobRunListParams::default())
        .expect("list runs");
    assert_eq!(
        runs.len(),
        1,
        "the rejected second click must not persist another run: {runs:?}"
    );
}

#[tokio::test]
async fn ship_endpoint_rejects_duplicate_task_ids() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    write_replay_job(&runtime, "task_auto_pipeline");

    let response = request_ship(
        runtime,
        Some(json!({ "task_ids": ["T12345678-123456", "T12345678-123456"] })),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let payload = body_json(response).await;
    assert!(
        payload["error"]
            .as_str()
            .is_some_and(|message| message.contains("duplicate task id"))
    );
}

// ─── child-dispatch lineage in run detail [ORB-10971] ─────────────────────

use orbit_types::workflow::{ChildDispatch, ChildDispatchPhase, PipelineState};

/// A parent run whose `ship_leaves` step is blocked on a durably linked child.
fn seed_parent_blocked_on_child(
    runtime: &OrbitRuntime,
    run_id: &str,
    run_state: JobRunState,
    phase: ChildDispatchPhase,
) -> JobRun {
    let run = seed_run(runtime, run_id, "workspace_auto_pipeline", run_state);
    write_seeded_run(runtime, &run);
    let mut state = PipelineState::new(
        run_id.to_string(),
        "workspace_auto_pipeline".to_string(),
        json!({}),
    );
    state.set_waiting_reasons(
        Some(vec!["ORB-1".to_string()]),
        Some(vec!["file:src/lib.rs".to_string()]),
    );
    state.record_child_dispatch(
        ChildDispatch::submitted(
            "jrun-child-leaves".to_string(),
            "task_auto_pipeline".to_string(),
            "invoke_and_wait".to_string(),
            true,
            false,
            Utc::now(),
        )
        .with_parent_step_id(Some("ship_leaves".to_string())),
    );
    state.advance_child_dispatch("jrun-child-leaves", phase, None, None);
    runtime
        .write_run_state(run_id, &state)
        .expect("seed parent run state");
    run
}

#[test]
fn run_detail_names_the_child_a_running_parent_is_blocked_on() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-web-child-waiting";
    let run = seed_parent_blocked_on_child(
        &runtime,
        run_id,
        JobRunState::Running,
        ChildDispatchPhase::Waiting,
    );
    assert_eq!(run.state, JobRunState::Running);
    let state = runtime
        .read_run_state(run_id)
        .expect("read run state")
        .expect("seeded run state");
    assert_eq!(state.waiting_on_deps, Some(vec!["ORB-1".to_string()]));

    let detail = job_run_detail_to_json(&runtime, &run);
    let dispatches = detail["run"]["child_dispatches"]
        .as_array()
        .expect("child_dispatches array");

    assert_eq!(dispatches.len(), 1);
    assert_eq!(dispatches[0]["child_run_id"], "jrun-child-leaves");
    assert_eq!(dispatches[0]["job_name"], "task_auto_pipeline");
    assert_eq!(dispatches[0]["parent_step_id"], "ship_leaves");
    assert_eq!(dispatches[0]["phase"], "waiting");
    assert_eq!(dispatches[0]["queued"], false);
    assert_eq!(detail["run"]["waiting_on_deps"], json!(["ORB-1"]));
    assert_eq!(
        detail["run"]["waiting_on_locks"],
        json!(["file:src/lib.rs"])
    );
    assert!(detail["run"].get("pid").is_none());
}

#[test]
fn run_detail_keeps_the_child_link_after_the_parent_terminalizes() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-web-child-cancelled";
    let run = seed_parent_blocked_on_child(
        &runtime,
        run_id,
        JobRunState::Cancelled,
        ChildDispatchPhase::Terminal,
    );

    let detail = job_run_detail_to_json(&runtime, &run);
    let dispatches = detail["run"]["child_dispatches"]
        .as_array()
        .expect("child_dispatches array");

    assert_eq!(
        dispatches.len(),
        1,
        "a cancelled parent must still name the child it left behind"
    );
    assert_eq!(dispatches[0]["child_run_id"], "jrun-child-leaves");
    assert_eq!(detail["run"]["waiting_on_deps"], Value::Null);
    assert_eq!(detail["run"]["waiting_on_locks"], Value::Null);
}

#[test]
fn a_terminal_run_never_projects_an_unfinished_step_as_still_running() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-web-cancelled-midstep";
    // A parent killed inside its dispatch wait: `step_started` was audited,
    // `step_finished` never was.
    seed_v2_audit_events(
        &runtime,
        run_id,
        vec![json!({
            "event_type": "step.started",
            "event_id": "evt-ship-leaves-started",
            "ts": "2026-08-22T19:55:00Z",
            "body_kind": "step_started",
            "step_id": "ship_leaves"
        })],
    );
    let run = seed_run(
        &runtime,
        run_id,
        "workspace_auto_pipeline",
        JobRunState::Cancelled,
    );
    write_seeded_run(&runtime, &run);

    let detail = job_run_detail_to_json(&runtime, &run);
    let steps = detail["steps"].as_array().expect("steps array");

    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0]["target_id"], "ship_leaves");
    assert_eq!(
        steps[0]["state"], "cancelled",
        "a step cannot outlive its own run"
    );
    assert_eq!(steps[0]["outcome"], "interrupted");
}

// ─── bounded auto-drain dashboard action [ORB-11250] ──────────────────────

mod auto_drain {
    use orbit_common::governance::authorization::OPERATOR_OVERRIDE_ENV;

    use super::*;

    async fn request_auto(runtime: OrbitRuntime, body: Option<Value>) -> Response {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri("/workflows/auto")
            .header(header::ORIGIN, "http://localhost:3000")
            .header(header::HOST, "localhost:3000");
        let body = match body {
            Some(json) => {
                builder = builder.header(header::CONTENT_TYPE, "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        router()
            .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
            .oneshot(builder.body(body).expect("request"))
            .await
            .expect("response")
    }

    async fn request_readiness(runtime: OrbitRuntime, query: &str) -> Response {
        router()
            .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
            .oneshot(
                Request::builder()
                    .uri(format!("/workflows/auto/readiness{query}"))
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response")
    }

    /// Pin the process signals `CallerCapabilities::resolve` reads, for the
    /// whole request. Mirrors `api::tests::auto_tasks`'s helper of the same
    /// shape: the guard in `orbit_common::test_env` is process-wide, so every
    /// case whose expected status depends on caller identity must go through
    /// it rather than reading/setting the env var directly [ORB-10894].
    #[allow(clippy::await_holding_lock)]
    async fn with_caller_env<'a, T>(
        vars: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
        fut: impl std::future::Future<Output = T>,
    ) -> T {
        let _env = orbit_common::test_env::scoped(vars);
        fut.await
    }

    async fn as_operator<T>(fut: impl std::future::Future<Output = T>) -> T {
        with_caller_env([(OPERATOR_OVERRIDE_ENV, Some("1"))], fut).await
    }

    async fn as_agent<T>(fut: impl std::future::Future<Output = T>) -> T {
        with_caller_env(
            [
                (OPERATOR_OVERRIDE_ENV, None),
                ("ORBIT_AGENT_NAME", Some("orbit-web-test")),
                ("ORBIT_AGENT_MODEL", Some("orbit-web-test")),
            ],
            fut,
        )
        .await
    }

    #[tokio::test]
    async fn auto_endpoint_rejects_missing_duration() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");

        let response = request_auto(runtime, None).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = body_json(response).await;
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|message| message.contains("for_duration"))
        );
    }

    #[tokio::test]
    async fn auto_endpoint_rejects_invalid_duration_format() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");

        let response = request_auto(runtime, Some(json!({ "for_duration": "soon" }))).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = body_json(response).await;
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|message| message.contains("invalid duration"))
        );
    }

    #[tokio::test]
    async fn auto_endpoint_rejects_zero_length_window() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");

        let response = request_auto(runtime, Some(json!({ "for_duration": "0s" }))).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = body_json(response).await;
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|message| message.contains("bounded window"))
        );
    }

    #[tokio::test]
    async fn auto_endpoint_rejects_zero_concurrency() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        write_replay_job(&runtime, "workspace_auto_pipeline");

        let response = request_auto(
            runtime,
            Some(json!({ "for_duration": "30m", "concurrency": 0 })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = body_json(response).await;
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|message| message.contains("concurrency must be at least 1"))
        );
    }

    /// Default completion (no `complete` opt-in) needs no authorization, the
    /// same as the ship endpoint's always-review submission.
    #[tokio::test]
    async fn auto_endpoint_defaults_to_review_without_authorization() {
        as_agent(async {
            let runtime = OrbitRuntime::in_memory().expect("build runtime");
            write_replay_job(&runtime, "workspace_auto_pipeline");

            let response = request_auto(runtime, Some(json!({ "for_duration": "30m" }))).await;

            assert_eq!(response.status(), StatusCode::OK);
            let payload = body_json(response).await;
            assert_eq!(payload["completion"].as_str(), Some("review"));
            assert!(payload["run_id"].as_str().is_some_and(|id| !id.is_empty()));
        })
        .await;
    }

    /// Opting into `complete` is separately governed: an unauthorized caller
    /// is refused before any run is submitted.
    #[tokio::test]
    async fn auto_endpoint_refuses_complete_opt_in_without_authorization() {
        as_agent(async {
            let runtime = OrbitRuntime::in_memory().expect("build runtime");
            write_replay_job(&runtime, "workspace_auto_pipeline");

            let response = request_auto(
                runtime.clone(),
                Some(json!({ "for_duration": "30m", "complete": true })),
            )
            .await;

            assert_eq!(response.status(), StatusCode::FORBIDDEN);
            let payload = body_json(response).await;
            assert_eq!(payload["code"].as_str(), Some("authorization_denied"));

            let runs = runtime
                .list_job_runs(JobRunListParams::default())
                .expect("list runs");
            assert!(
                runs.is_empty(),
                "a denied completion opt-in must not submit a run: {runs:?}"
            );
        })
        .await;
    }

    /// An authorized operator's `complete` opt-in propagates to the same
    /// `CompletionPolicy::Done` the CLI's `--complete` sends.
    #[tokio::test]
    async fn auto_endpoint_propagates_authorized_complete_opt_in() {
        as_operator(async {
            let runtime = OrbitRuntime::in_memory().expect("build runtime");
            write_replay_job(&runtime, "workspace_auto_pipeline");

            let response = request_auto(
                runtime.clone(),
                Some(json!({ "for_duration": "2h", "complete": true })),
            )
            .await;

            assert_eq!(response.status(), StatusCode::OK);
            let payload = body_json(response).await;
            assert_eq!(payload["completion"].as_str(), Some("done"));
            let run_id = payload["run_id"].as_str().expect("run_id").to_string();

            let run = runtime.show_job_run(&run_id).expect("show run");
            let input = run.input.expect("run input");
            assert_eq!(input["completion"], "done");
            assert_eq!(input["for_seconds"], 7200);
        })
        .await;
    }

    /// ORB-10008: an unknown `?workspace=` is a clean 404 JSON rejection from
    /// the workspace extractor, matching the ship endpoint's behavior — this
    /// is how the endpoint refuses all-workspace mode too, since aggregate
    /// state has no default workspace to fall back on.
    #[tokio::test]
    async fn auto_endpoint_rejects_unknown_workspace_with_404_json() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");
        let state = crate::state::DashboardState::single(Arc::new(runtime));

        let response = router()
            .with_state(state)
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/workflows/auto?workspace=ghost")
                    .header(header::ORIGIN, "http://localhost:7878")
                    .header(header::HOST, "localhost:7878")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({ "for_duration": "30m" }).to_string()))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let payload = body_json(response).await;
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|m| m.contains("unknown workspace: ghost"))
        );
    }

    #[tokio::test]
    async fn readiness_endpoint_returns_read_only_snapshot() {
        let runtime = OrbitRuntime::in_memory().expect("build runtime");

        let response = request_readiness(runtime, "").await;

        assert_eq!(response.status(), StatusCode::OK);
        let payload = body_json(response).await;
        assert_eq!(payload["snapshot"]["read_only"], true);
        assert!(payload["capacity"]["free_slots"].is_number());
        assert!(payload["controls_authorized"].is_boolean());
    }
}

fn find_bundle_lock(root: &std::path::Path, task_id: &str) -> Option<std::path::PathBuf> {
    let name = format!(".{task_id}.bundle.lock");
    fn walk(dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
        for entry in std::fs::read_dir(dir).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.file_name().and_then(|n| n.to_str()) == Some(name) {
                return Some(path);
            }
            if path.is_dir()
                && let Some(found) = walk(&path, name)
            {
                return Some(found);
            }
        }
        None
    }
    walk(root, &name)
}

#[cfg(unix)]
fn hold_exclusive_bundle_lock(path: &std::path::Path) -> std::fs::File {
    use std::os::unix::io::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open bundle lock");
    // SAFETY: `file` is open for the lifetime of this binding; `LOCK_EX` is a
    // valid flock operation on that descriptor.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    assert_eq!(rc, 0, "exclusive flock on {}", path.display());
    file
}

async fn raw_http(
    addr: std::net::SocketAddr,
    request: &str,
) -> Result<(u16, Vec<u8>), std::io::Error> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .unwrap_or(buf.len());
    let header = std::str::from_utf8(&buf[..header_end]).unwrap_or("");
    let status = header
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((status, buf[header_end..].to_vec()))
}

/// Holding the task-bundle flock parks `submit_ship_run` (it `get_task`s under
/// that lock). Sixteen concurrent ships must not starve `/healthz`.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn healthz_answers_within_one_second_while_sixteen_ships_wait_on_bundle_lock() {
    use std::time::Duration;

    use axum::routing::get;

    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let task_id = runtime
        .add_task(TaskAddParams {
            title: "ship lock fixture".to_string(),
            description: "held under exclusive bundle flock during concurrent ship".to_string(),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .expect("seed task")
        .id;
    let lock_path = find_bundle_lock(&runtime.data_root(), &task_id)
        .or_else(|| find_bundle_lock(&runtime.shared_root(), &task_id))
        .unwrap_or_else(|| {
            panic!(
                "missing .{task_id}.bundle.lock under {} or {}",
                runtime.data_root().display(),
                runtime.shared_root().display()
            )
        });
    let _guard = hold_exclusive_bundle_lock(&lock_path);

    let state = crate::state::DashboardState::single(Arc::new(runtime));
    let app = axum::Router::new()
        .route("/healthz", get(crate::health::healthz))
        .nest("/api", router())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve dashboard");
    });

    let ship_body = json!({ "task_ids": [task_id], "mode": "local" }).to_string();
    let ship_request = format!(
        "POST /api/workflows/ship HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:3000\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        ship_body.len(),
        ship_body
    );
    let mut ships = Vec::new();
    for _ in 0..16 {
        let request = ship_request.clone();
        ships.push(tokio::spawn(async move { raw_http(addr, &request).await }));
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    let health = tokio::time::timeout(
        Duration::from_secs(1),
        raw_http(
            addr,
            "GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        ),
    )
    .await
    .expect("healthz timed out")
    .expect("healthz request");
    assert_eq!(health.0, 200, "healthz status");
    assert_eq!(health.1, b"ok", "healthz body");

    drop(_guard);
    for ship in ships {
        let _ = tokio::time::timeout(Duration::from_secs(5), ship).await;
    }
}
