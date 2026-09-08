//! Test-only allowlist: the original tests under orbit-cli passed the same lints via
//! the crate-level test harness configuration; duplicated here for the extracted crate.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use chrono::Utc;
use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use orbit_core::application::task::TaskAddParams;
use orbit_core::{
    InvocationInsertParams, OrbitRuntime, TaskComplexity, TaskStatus, V2AuditEventInsertParams,
};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use serde_json::json;
use tempfile::tempdir;
use tower::ServiceExt;

use super::super::diagnostics::{
    diagnostics_friction_row, diagnostics_metrics_values, global_error_rows_from_path,
    parse_structured_error_line,
};
use super::super::router;
use super::test_support::{body_json, write_lines};
use orbit_common::storage::blob_store::BlobStore;

async fn request_dashboard_errors(runtime: OrbitRuntime) -> Response {
    Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri("/api/diagnostics/errors?limit=10")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

fn seed_cli_invocation_audit(runtime: &OrbitRuntime, run_id: &str, stderr: &[u8]) -> String {
    seed_cli_invocation_audit_at(runtime, run_id, stderr, 4)
}

/// Seed one run's `run.started` → `step.started` → `cli.invocation.finished`
/// envelope at `2099-05-08T<hour>:12:2xZ`, so several runs can be ordered.
fn seed_cli_invocation_audit_at(
    runtime: &OrbitRuntime,
    run_id: &str,
    stderr: &[u8],
    hour: u32,
) -> String {
    let audit_root = runtime.data_root().join("state").join("audit");
    let blob_store = BlobStore::new(audit_root.join("blobs"));
    let stdout_ref = blob_store
        .write(b"normal output\n")
        .expect("write stdout blob");
    let stderr_ref = blob_store.write(stderr).expect("write stderr blob");
    let workspace_id = runtime.workspace_id().expect("workspace id");
    for event in [
        json!({
            "schemaVersion": 1,
            "event_type": "run.started",
            "event_id": format!("evt-run-{run_id}"),
            "ts": format!("2099-05-08T{hour:02}:12:20Z"),
            "run_id": run_id,
            "agent_identity": "system",
            "body_kind": "run_started"
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "step.started",
            "event_id": format!("evt-step-{run_id}"),
            "ts": format!("2099-05-08T{hour:02}:12:21Z"),
            "run_id": run_id,
            "agent_identity": "system",
            "parent_event_id": format!("evt-run-{run_id}"),
            "body_kind": "step_started",
            "step_id": "implement"
        }),
        json!({
            "schemaVersion": 1,
            "event_type": "cli.invocation.finished",
            "event_id": format!("evt-cli-{run_id}"),
            "ts": format!("2099-05-08T{hour:02}:12:22Z"),
            "run_id": run_id,
            "agent_identity": "system",
            "parent_event_id": format!("evt-step-{run_id}"),
            "body_kind": "cli_invocation_finished",
            "provider": "codex",
            "stdout_blob_ref": stdout_ref,
            "stderr_blob_ref": stderr_ref,
            "exit_code": 0,
            "timed_out": false,
            "duration_ms": 123
        }),
    ] {
        let ts = event["ts"]
            .as_str()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            .map(|value| value.with_timezone(&Utc))
            .expect("event ts");
        runtime
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: workspace_id.clone(),
                event_id: event["event_id"].as_str().expect("event id").to_string(),
                source: "v2_envelope".to_string(),
                schema_version: 1,
                event_type: event["event_type"]
                    .as_str()
                    .expect("event type")
                    .to_string(),
                ts,
                run_id: run_id.to_string(),
                agent_identity: "system".to_string(),
                parent_event_id: event
                    .get("parent_event_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                workspace_path: None,
                payload_json: event.to_string(),
            })
            .expect("insert v2 audit event");
    }
    stderr_ref
}

#[test]
fn diagnostics_metrics_values_adapt_invocation_records() {
    let ts = chrono::DateTime::parse_from_rfc3339("2026-05-05T03:29:45Z")
        .expect("parse timestamp")
        .with_timezone(&Utc);
    let rows = diagnostics_metrics_values(vec![orbit_core::InvocationRecord {
        id: 7,
        ts,
        job_run_id: "jrun-1".to_string(),
        activity_id: "implement_one".to_string(),
        agent: "codex".to_string(),
        model: Some(TEST_CODEX_MODEL.to_string()),
        duration_ms: 1234,
        input_tokens: 100,
        cache_read_tokens: 0,
        cache_create_tokens: 0,
        cache_create_1h_tokens: 0,
        output_tokens: 23,
        total_tokens: 123,
        tool_call_count: 4,
        task_ids: vec!["T20260505-1".to_string()],
        tool_calls: Vec::new(),
        provider_cost_usd: None,
        derived_cost_usd: None,
    }]);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["step"], "implement_one");
    assert_eq!(rows[0]["actor_identity"], "codex / gpt-5.5");
    assert_eq!(rows[0]["token_usage"], 123);
    assert_eq!(rows[0]["tool_invocations"], 4);
    assert_eq!(rows[0]["step_duration_ms"], 1234);
    assert_eq!(rows[0]["task_id"], "T20260505-1");
}

#[test]
fn diagnostics_friction_row_extracts_failed_cli_stderr_and_step() {
    let dir = tempdir().expect("tempdir");
    let blob_store = BlobStore::new(dir.path());
    let stderr_ref = blob_store.write(b"command failed\n").expect("write blob");

    let step = json!({
        "event_id": "evt-step",
        "body_kind": "step_started",
        "step_id": "implement_one"
    });
    let activity = json!({
        "event_id": "evt-activity",
        "body_kind": "activity_started",
        "parent_event_id": "evt-step"
    });
    let event = json!({
        "event_id": "evt-cli",
        "ts": "2026-05-05T03:29:45Z",
        "run_id": "jrun-1",
        "agent_identity": "system",
        "body_kind": "cli_invocation_finished",
        "parent_event_id": "evt-activity",
        "provider": "codex",
        "exit_code": 1,
        "stderr_blob_ref": stderr_ref,
        "timed_out": false
    });
    let events_by_id = HashMap::from([
        ("evt-step".to_string(), step),
        ("evt-activity".to_string(), activity),
        ("evt-cli".to_string(), event.clone()),
    ]);

    let row = diagnostics_friction_row(&blob_store, &events_by_id, &event, "2026-05").expect("row");

    assert_eq!(row["step"], "implement_one");
    assert_eq!(row["command"], "codex");
    assert_eq!(row["exit_code"], 1);
    assert_eq!(row["stderr"], "command failed\n");
}

#[tokio::test]
async fn diagnostics_errors_include_codex_style_stderr_rows() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let run_id = "jrun-error-api";
    // The endpoint merges global process-log errors with fixture audit errors before truncating.
    let stderr = b"2099-05-08T04:12:22.346005Z ERROR codex_core::session: failed to record rollout items\nordinary stderr\nERROR codex_core::tools::router: apply_patch verification failed\n";
    let stderr_ref = seed_cli_invocation_audit(&runtime, run_id, stderr);

    let response = request_dashboard_errors(runtime).await;

    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    let rows = payload.as_array().expect("rows");
    let agent_rows = rows
        .iter()
        .filter(|row| row["source"] == "agent-stderr" && row["job_run"] == run_id)
        .collect::<Vec<_>>();
    assert_eq!(agent_rows.len(), 2);
    assert_eq!(agent_rows[0]["job_run"], run_id);
    assert_eq!(agent_rows[0]["step"], "implement");
    assert_eq!(agent_rows[0]["step_index"], 0);
    assert_eq!(agent_rows[0]["provider"], "codex");
    assert_eq!(agent_rows[0]["blob_ref"], stderr_ref);
    assert!(agent_rows[0]["affiliation"].is_null());
    assert!(rows.iter().any(|row| {
        row["message"]
            .as_str()
            .is_some_and(|message| message.contains("apply_patch verification failed"))
    }));
}

/// The scan stops after `2 * limit` stderr rows, so it has to walk history
/// newest-first: walking oldest-first fills that budget with the first runs
/// ever recorded and a failure from five minutes ago never reaches the panel.
#[tokio::test]
async fn diagnostics_errors_prefer_the_newest_agent_stderr_rows() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    for (run_id, hour) in [("jrun-old", 1), ("jrun-mid", 2), ("jrun-new", 3)] {
        let stderr = format!(
            "2099-05-08T{hour:02}:12:22.000000Z ERROR codex_core::session: failed in {run_id}\n"
        );
        seed_cli_invocation_audit_at(&runtime, run_id, stderr.as_bytes(), hour);
    }

    let response = Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri("/api/diagnostics/errors?limit=1")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let payload = body_json(response).await;
    let rows = payload.as_array().expect("rows");
    let agent_rows = rows
        .iter()
        .filter(|row| row["source"] == "agent-stderr")
        .collect::<Vec<_>>();
    assert_eq!(agent_rows.len(), 1, "{payload}");
    assert_eq!(agent_rows[0]["job_run"], "jrun-new");
}

#[test]
fn global_error_rows_include_process_log_errors() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.log.jsonl");
    write_lines(
        &path,
        &[
            json!({
                "timestamp": "2026-05-08T04:00:00Z",
                "level": "INFO",
                "target": "orbit.test",
                "fields": { "message": "ignored" }
            })
            .to_string(),
            json!({
                "timestamp": "2026-05-08T04:01:00Z",
                "level": "ERROR",
                "target": "orbit.test",
                "fields": { "message": "process failed" }
            })
            .to_string(),
        ],
    );

    let rows = global_error_rows_from_path(&path, 10).expect("rows");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["ts"], "2026-05-08T04:01:00Z");
    assert_eq!(rows[0]["source"], "process");
    assert_eq!(rows[0]["job_run"], serde_json::Value::Null);
    assert_eq!(rows[0]["step"], serde_json::Value::Null);
    assert_eq!(rows[0]["task_id"], serde_json::Value::Null);
    assert_eq!(rows[0]["provider"], serde_json::Value::Null);
    assert_eq!(rows[0]["affiliation"], "unaffiliated");
    assert!(
        rows[0]["message"]
            .as_str()
            .is_some_and(|message| message.contains("process failed"))
    );
}

#[test]
fn global_error_rows_copy_run_attribution_from_process_log() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.log.jsonl");
    write_lines(
        &path,
        &[
            json!({
                "timestamp": "2026-05-08T04:02:00Z",
                "level": "ERROR",
                "target": "orbit.job.step_finished",
                "fields": {
                    "message": "step finished",
                    "job_run_id": "jrun-20260906-0306-2",
                    "task_id": "ORB-11398",
                    "step_id": "require_apply_success",
                    "provider": "codex",
                    "outcome": "failed",
                    "success": false
                }
            })
            .to_string(),
            json!({
                "timestamp": "2026-05-08T04:03:00Z",
                "level": "ERROR",
                "target": "orbit.core.job_run",
                "fields": {
                    "message": "failed to observe pipeline worker startup",
                    "run_id": "jrun-20260906-0204-9",
                    "task_id": "ORB-11331",
                    "step": "starvation_check"
                }
            })
            .to_string(),
        ],
    );

    let rows = global_error_rows_from_path(&path, 10).expect("rows");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["source"], "process");
    assert_eq!(rows[0]["job_run"], "jrun-20260906-0306-2");
    assert_eq!(rows[0]["task_id"], "ORB-11398");
    assert_eq!(rows[0]["step"], "require_apply_success");
    assert_eq!(rows[0]["provider"], "codex");
    assert_eq!(rows[0]["affiliation"], "run");
    assert_eq!(rows[1]["source"], "process");
    assert_eq!(rows[1]["job_run"], "jrun-20260906-0204-9");
    assert_eq!(rows[1]["task_id"], "ORB-11331");
    assert_eq!(rows[1]["step"], "starvation_check");
    assert_eq!(rows[1]["provider"], serde_json::Value::Null);
    assert_eq!(rows[1]["affiliation"], "run");
}

#[test]
fn global_error_rows_prefer_job_run_id_over_run_id() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.log.jsonl");
    write_lines(
        &path,
        &[json!({
            "timestamp": "2026-05-08T04:04:00Z",
            "level": "ERROR",
            "target": "orbit.test",
            "fields": {
                "message": "both run keys present",
                "job_run_id": "jrun-canonical",
                "run_id": "jrun-alias"
            }
        })
        .to_string()],
    );

    let rows = global_error_rows_from_path(&path, 10).expect("rows");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["job_run"], "jrun-canonical");
    assert_eq!(rows[0]["affiliation"], "run");
}

#[test]
fn parse_structured_error_line_ignores_unstructured_error_words() {
    assert!(parse_structured_error_line("this has ERROR but no shape", "").is_none());
    let parsed = parse_structured_error_line(
        "2026-05-08T04:12:22.346005Z ERROR codex_core::session: failed",
        "",
    )
    .expect("parsed");
    assert_eq!(parsed.ts, "2026-05-08T04:12:22.346005Z");
    assert_eq!(parsed.target, "codex_core::session");
    assert_eq!(parsed.message, "failed");
}

fn seed_task(
    runtime: &OrbitRuntime,
    title: &str,
    status: TaskStatus,
    complexity: Option<TaskComplexity>,
) -> orbit_core::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: format!("Fixture task: {title}."),
            status: Some(status),
            // `TaskAddParams::complexity` became required at create time
            // [ORB-10892]; `None` here is the fixture's "nothing was assessed",
            // which that surface spells `Unassessed` and which the reporting
            // buckets fold into `unset` [ORB-10895].
            complexity: complexity.unwrap_or(TaskComplexity::Unassessed),
            workspace_path: Some(".".to_string()),
            ..Default::default()
        })
        .expect("seed task")
}

async fn get_json(runtime: OrbitRuntime, uri: &str) -> serde_json::Value {
    let response = Router::new()
        .nest("/api", router())
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

#[tokio::test]
async fn completion_by_complexity_keeps_unset_and_shows_denominators() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_task(
        &runtime,
        "Hard done",
        TaskStatus::Done,
        Some(TaskComplexity::Hard),
    );
    let hard_archived = seed_task(
        &runtime,
        "Hard archived",
        TaskStatus::Backlog,
        Some(TaskComplexity::Hard),
    );
    runtime
        .archive_task(&hard_archived.id)
        .expect("archive hard task");
    seed_task(
        &runtime,
        "Medium rejected",
        TaskStatus::Rejected,
        Some(TaskComplexity::Medium),
    );
    seed_task(&runtime, "Unlabeled backlog", TaskStatus::Backlog, None);

    let payload = get_json(runtime, "/api/tasks/completion-by-complexity").await;
    let buckets = payload["by_complexity"]
        .as_array()
        .expect("by_complexity array");
    let unset = buckets
        .iter()
        .find(|row| row["complexity"] == "unset")
        .expect("unset bucket");
    assert_eq!(unset["total"], 1);
    let statuses = unset["statuses"].as_array().expect("statuses");
    for required in ["done", "rejected", "archived"] {
        let row = statuses
            .iter()
            .find(|status| status["status"] == required)
            .unwrap_or_else(|| panic!("missing {required}"));
        assert_eq!(row["total"], 1, "{required} states its denominator");
    }
    let backlog = statuses
        .iter()
        .find(|status| status["status"] == "backlog")
        .expect("backlog counted");
    assert_eq!(backlog["count"], 1);

    let hard = buckets
        .iter()
        .find(|row| row["complexity"] == "hard")
        .expect("hard bucket");
    assert_eq!(hard["total"], 2);
    let hard_archived = hard["statuses"]
        .as_array()
        .expect("hard statuses")
        .iter()
        .find(|status| status["status"] == "archived")
        .expect("hard archived");
    assert_eq!(hard_archived["count"], 1);
    assert_eq!(hard_archived["total"], 2);
}

#[tokio::test]
async fn implement_one_duration_is_faceted_by_complexity() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let hard = seed_task(
        &runtime,
        "Hard implement",
        TaskStatus::InProgress,
        Some(TaskComplexity::Hard),
    );
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-hard".to_string(),
            activity_id: "implement_one".to_string(),
            agent: "codex".to_string(),
            model: Some(TEST_CODEX_MODEL.to_string()),
            task_ids: vec![hard.id.clone()],
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 10,
                    cache_read: 0,
                    cache_create: 0,
                    cache_create_1h: 0,
                    output: 4,
                },
                tool_calls: Vec::new(),
                duration_ms: 4_000,
                provider_model: None,
                provider_cost_usd: None,
            },
        })
        .expect("insert hard invocation");
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: "jrun-unset".to_string(),
            activity_id: "implement_one".to_string(),
            agent: "claude".to_string(),
            model: Some("claude-opus-5".to_string()),
            task_ids: Vec::new(),
            trace: InvocationTrace {
                usage: TokenUsage {
                    input: 8,
                    cache_read: 0,
                    cache_create: 0,
                    cache_create_1h: 0,
                    output: 2,
                },
                tool_calls: Vec::new(),
                duration_ms: 1_000,
                provider_model: None,
                provider_cost_usd: None,
            },
        })
        .expect("insert unlabeled invocation");

    let payload = get_json(runtime, "/api/diagnostics/implement_one").await;
    let bands = payload["implement_one_by_complexity"]
        .as_array()
        .expect("faceted bands");
    let hard_band = bands
        .iter()
        .find(|band| band["complexity"] == "hard")
        .expect("hard band");
    assert_eq!(hard_band["n"], 1);
    assert_eq!(hard_band["actors"][0]["n"], 1);
    assert_eq!(hard_band["actors"][0]["avg"], 4000.0);

    let unset_band = bands
        .iter()
        .find(|band| band["complexity"] == "unset")
        .expect("unset band stays visible");
    assert_eq!(unset_band["n"], 1);
    assert_eq!(unset_band["actors"][0]["avg"], 1000.0);
}
