//! Endpoint tests for `GET /audit/incidents` [ORB-10871].
//!
//! The endpoint's job is to report *two* numbers over one window — how many
//! distinct problems occurred, and how much raw evidence they collapsed —
//! without either being inferable from the other and without removing any row
//! from the raw audit surface. Fixtures below are synthetic; no observed actor,
//! tool, workspace, task, or event id appears.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_core::{AuditEventInsertParams, AuditEventStatus, OrbitRuntime};
use serde_json::Value;
use tower::ServiceExt;

use super::super::router;
use super::test_support::body_json;

#[allow(clippy::too_many_arguments)]
fn seed(
    runtime: &OrbitRuntime,
    execution_id: &str,
    tool_name: &str,
    status: AuditEventStatus,
    role: &str,
    error_message: Option<&str>,
    job_run_id: Option<&str>,
) {
    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: execution_id.to_string(),
            command: "tool".to_string(),
            subcommand: Some("run".to_string()),
            tool_name: Some(tool_name.to_string()),
            target_type: Some("tool".to_string()),
            target_id: Some(tool_name.to_string()),
            role: role.to_string(),
            status,
            exit_code: match status {
                AuditEventStatus::Success => 0,
                _ => 1,
            },
            duration_ms: 4,
            working_directory: "/tmp/fixture".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: error_message.map(str::to_string),
            host: None,
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: job_run_id.map(str::to_string),
            activity_id: None,
            step_index: None,
        })
        .expect("seed audit event");
}

#[allow(clippy::too_many_arguments)]
fn seed_lifecycle(
    runtime: &OrbitRuntime,
    execution_id: &str,
    command: &str,
    status: AuditEventStatus,
    role: &str,
    error_message: Option<&str>,
    job_run_id: Option<&str>,
    activity_id: Option<&str>,
    task_id: Option<&str>,
) {
    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: execution_id.to_string(),
            command: command.to_string(),
            subcommand: (command != "Start").then(|| "run".to_string()),
            tool_name: None,
            target_type: Some("job_run".to_string()),
            target_id: job_run_id.map(str::to_string),
            role: role.to_string(),
            status,
            exit_code: 1,
            duration_ms: 4,
            working_directory: "/tmp/fixture".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: error_message.map(str::to_string),
            host: None,
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: task_id.map(str::to_string),
            job_run_id: job_run_id.map(str::to_string),
            activity_id: activity_id.map(str::to_string),
            step_index: None,
        })
        .expect("seed lifecycle audit event");
}

fn seed_ten_unknown_lifecycle_rows(runtime: &OrbitRuntime) {
    seed_lifecycle(
        runtime,
        "exec-start-1",
        "Start",
        AuditEventStatus::Failure,
        "actor-one",
        Some("job run start failed"),
        None,
        None,
        None,
    );
    seed_lifecycle(
        runtime,
        "exec-start-2",
        "Start",
        AuditEventStatus::Failure,
        "actor-one",
        Some("job run start failed"),
        None,
        None,
        None,
    );
    for index in 0..4 {
        let leaf = format!("jrun-leaf-{index}");
        let parent = format!("jrun-parent-{index}");
        seed_lifecycle(
            runtime,
            &format!("exec-leaf-{index}"),
            "job",
            AuditEventStatus::Failure,
            "actor-one",
            Some("child step returned a nonzero status"),
            Some(&leaf),
            Some("leaf-step"),
            Some(&format!("REC-leaf-{index}")),
        );
        seed_lifecycle(
            runtime,
            &format!("exec-parent-{index}"),
            "job",
            AuditEventStatus::Failure,
            "actor-one",
            Some(&format!(
                "pipeline child run did not succeed: result run {leaf} status failed: child step returned a nonzero status"
            )),
            Some(&parent),
            Some("pipeline_success_guard"),
            Some(&format!("REC-parent-{index}")),
        );
    }
}

fn seed_named_diagnostic(
    runtime: &OrbitRuntime,
    execution_id: &str,
    tool_name: &str,
    job_run_id: &str,
    task_id: &str,
) {
    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: execution_id.to_string(),
            command: "tool".to_string(),
            subcommand: Some("run".to_string()),
            tool_name: Some(tool_name.to_string()),
            target_type: Some("job_run".to_string()),
            target_id: Some(job_run_id.to_string()),
            role: "admin".to_string(),
            status: AuditEventStatus::Failure,
            exit_code: 1,
            duration_ms: 0,
            working_directory: "/tmp/fixture".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: Some("pipeline worker lifecycle diagnostic".to_string()),
            host: None,
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: Some(task_id.to_string()),
            job_run_id: Some(job_run_id.to_string()),
            activity_id: Some("worker-observer".to_string()),
            step_index: None,
        })
        .expect("seed named diagnostic");
}

async fn request(runtime: OrbitRuntime, uri: &str) -> axum::response::Response {
    router()
        .with_state(crate::state::DashboardState::single(Arc::new(runtime)))
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .header("host", "localhost:7878")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

/// One burst plus one unrelated failure plus one success: the incident count,
/// the raw failed count, and the all-events denominator must each be reported
/// on their own.
#[tokio::test]
async fn incidents_report_grouped_and_raw_counts_against_stated_denominators() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for index in 0..40 {
        seed(
            &runtime,
            &format!("exec-burst-{index}"),
            "surface.alpha",
            AuditEventStatus::Failure,
            "actor-one",
            Some(&format!("could not remove /work/dir-{index}/file.txt")),
            None,
        );
    }
    seed(
        &runtime,
        "exec-other",
        "surface.beta",
        AuditEventStatus::Failure,
        "actor-one",
        Some("connection reset by peer"),
        None,
    );
    seed(
        &runtime,
        "exec-ok",
        "surface.alpha",
        AuditEventStatus::Success,
        "actor-one",
        None,
        None,
    );

    let response = request(runtime, "/audit/incidents?since=24h").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["window"], "24h", "the selected window is echoed back");
    assert!(body["since"].is_string());
    assert_eq!(body["incident_count"].as_u64(), Some(2));
    assert_eq!(
        body["raw_failed_events"].as_u64(),
        Some(41),
        "raw failure evidence is preserved, not collapsed away"
    );
    assert_eq!(
        body["total_events"].as_u64(),
        Some(42),
        "the failed count states what it is out of"
    );

    let incidents = body["incidents"].as_array().expect("incidents array");
    let burst = incidents
        .iter()
        .find(|incident| incident["surface"] == "surface.alpha")
        .expect("burst incident");
    assert_eq!(burst["event_count"].as_u64(), Some(40));
    assert_eq!(burst["class"], "unexpected");
    assert_eq!(burst["class_label"], "unexpected failure");
    assert!(
        burst["signature"]
            .as_str()
            .is_some_and(|s| s.contains("<path>")),
        "the grouping signature is exposed so the collapse is explainable"
    );
    assert!(
        !burst["sample_events"]
            .as_array()
            .expect("sample events")
            .is_empty(),
        "the exact underlying rows must be reachable from the incident"
    );
    let sample = &burst["sample_events"].as_array().expect("samples")[0];
    for field in ["id", "ts", "execution_id", "status", "actor", "surface"] {
        assert!(!sample[field].is_null(), "sample event must carry {field}");
    }
}

/// A cascade in one run is one incident with its chain attached; the raw rows
/// of every link stay counted and addressable.
#[tokio::test]
async fn a_run_cascade_is_one_incident_with_its_propagation_chain() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for (execution_id, surface, message) in [
        (
            "exec-inner",
            "step.inner",
            "child step returned a nonzero status",
        ),
        (
            "exec-middle",
            "step.middle",
            "bundle aborted after a child failure",
        ),
        ("exec-gate", "step.gate", "gate refused to advance"),
    ] {
        seed(
            &runtime,
            execution_id,
            surface,
            AuditEventStatus::Failure,
            "actor-one",
            Some(message),
            Some("run-alpha"),
        );
    }

    let response = request(runtime, "/audit/incidents?since=24h").await;
    let body = body_json(response).await;

    assert_eq!(
        body["incident_count"].as_u64(),
        Some(1),
        "one failed run must not read as three independent root causes"
    );
    assert_eq!(body["raw_failed_events"].as_u64(), Some(3));
    let incident = &body["incidents"].as_array().expect("incidents")[0];
    assert_eq!(incident["event_count"].as_u64(), Some(3));
    assert_eq!(incident["root_event_count"].as_u64(), Some(1));
    assert_eq!(incident["propagated_event_count"].as_u64(), Some(2));
    assert_eq!(incident["run_ids"][0], "run-alpha");
    let chain: Vec<&str> = incident["propagation"]
        .as_array()
        .expect("propagation")
        .iter()
        .filter_map(|link| link["surface"].as_str())
        .collect();
    assert_eq!(chain, vec!["step.middle", "step.gate"]);
}

/// Denials, expected negative paths, and unexpected failures are counted and
/// labeled separately, and every class stays selectable.
#[tokio::test]
async fn failure_classes_are_reported_separately_and_are_filterable() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    seed(
        &runtime,
        "exec-denied",
        "surface.alpha",
        AuditEventStatus::Denied,
        "actor-one",
        Some("policy denied: write outside the allowed scope"),
        None,
    );
    seed(
        &runtime,
        "exec-expected",
        "surface.alpha",
        AuditEventStatus::Failure,
        "actor-one",
        Some("invalid input: field must not be empty"),
        None,
    );
    seed(
        &runtime,
        "exec-unexpected",
        "surface.alpha",
        AuditEventStatus::Failure,
        "actor-one",
        Some("internal channel closed unexpectedly"),
        None,
    );

    let body = body_json(request(runtime.clone(), "/audit/incidents?since=24h").await).await;
    assert_eq!(body["incident_count"].as_u64(), Some(3));
    assert_eq!(body["incidents_by_class"]["denied"].as_u64(), Some(1));
    assert_eq!(body["incidents_by_class"]["expected"].as_u64(), Some(1));
    assert_eq!(body["incidents_by_class"]["unexpected"].as_u64(), Some(1));
    assert_eq!(body["raw_events_by_class"]["denied"].as_u64(), Some(1));
    assert_eq!(
        body["class_labels"]["expected"], "expected negative path",
        "each class must carry an operator-facing label"
    );

    let filtered =
        body_json(request(runtime, "/audit/incidents?since=24h&class=unexpected").await).await;
    assert_eq!(
        filtered["matching_incident_count"].as_u64(),
        Some(1),
        "class filtering narrows the list"
    );
    assert_eq!(
        filtered["incident_count"].as_u64(),
        Some(3),
        "the unfiltered total stays visible so nothing looks dropped"
    );
    assert_eq!(
        filtered["incidents"].as_array().expect("incidents").len(),
        1
    );
}

/// Grouping is a derived view: the raw audit listing keeps every row.
#[tokio::test]
async fn grouping_does_not_remove_rows_from_the_raw_audit_view() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for index in 0..6 {
        seed(
            &runtime,
            &format!("exec-{index}"),
            "surface.alpha",
            AuditEventStatus::Failure,
            "actor-one",
            Some("operation failed"),
            None,
        );
    }

    let grouped = body_json(request(runtime.clone(), "/audit/incidents?since=24h").await).await;
    assert_eq!(grouped["incident_count"].as_u64(), Some(1));

    let raw: Value = body_json(request(runtime, "/audit?limit=100").await).await;
    assert_eq!(
        raw.as_array().expect("raw audit rows").len(),
        6,
        "the raw Audit view still returns every underlying event"
    );
}

#[tokio::test]
async fn the_lifetime_window_is_a_valid_scope() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let response = request(runtime, "/audit/incidents?since=all").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["window"], "all");
    assert!(
        body["since"].is_null(),
        "a lifetime window has no cutoff to state"
    );
}

#[tokio::test]
async fn an_unknown_failure_class_is_a_client_error() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    let response = request(runtime, "/audit/incidents?since=24h&class=bogus").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// ORB-10969: ten no-tool rows = 2 duplicate Starts + 8 cascade rows from 4
/// leaf failures. The incidents API must report grouped counts, the lifecycle
/// category, and every underlying row on expansion.
#[tokio::test]
async fn ten_unknown_lifecycle_rows_group_as_four_cascades_and_one_start() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    seed_ten_unknown_lifecycle_rows(&runtime);

    let body = body_json(request(runtime, "/audit/incidents?since=24h").await).await;

    assert_eq!(body["raw_failed_events"].as_u64(), Some(10));
    assert_eq!(body["incident_count"].as_u64(), Some(5));
    assert_eq!(body["affected_run_count"].as_u64(), Some(8));
    assert_eq!(body["job_run_lifecycle_events"].as_u64(), Some(10));
    assert_eq!(body["job_run_lifecycle_incidents"].as_u64(), Some(5));
    assert_eq!(body["job_run_lifecycle_label"], "job-run lifecycle");

    let incidents = body["incidents"].as_array().expect("incidents");
    assert_eq!(incidents.len(), 5);
    let start = incidents
        .iter()
        .find(|incident| incident["surface"] == "Start")
        .expect("Start incident");
    assert_eq!(start["event_count"].as_u64(), Some(2));
    assert_eq!(start["has_tool_identity"], false);

    let cascades: Vec<_> = incidents
        .iter()
        .filter(|incident| incident["surface"] != "Start")
        .collect();
    assert_eq!(cascades.len(), 4);
    for incident in cascades {
        assert_eq!(incident["root_event_count"].as_u64(), Some(1));
        assert_eq!(incident["propagated_event_count"].as_u64(), Some(1));
        let events = incident["events"].as_array().expect("full event list");
        assert_eq!(events.len(), 2, "expansion keeps every underlying row");
        for event in events {
            assert!(
                event["tool"].is_null(),
                "lifecycle rows have no tool identity"
            );
            assert!(event["run_id"].as_str().is_some(), "run id is present");
            assert!(event["task_id"].as_str().is_some(), "task id is present");
        }
    }
}

/// ORB-11118: seven duplicate-worker incidents emit fourteen raw diagnostic
/// rows. They remain fully expandable with run/task/tool evidence while their
/// class-specific denominator stays separate from callable reliability.
#[tokio::test]
async fn named_worker_diagnostics_report_seven_incidents_and_fourteen_exact_rows() {
    let runtime = OrbitRuntime::in_memory().expect("runtime");
    for index in 0..7 {
        let run_id = format!("jrun-diagnostic-{index}");
        let task_id = format!("REC-diagnostic-{index}");
        let tool = if index % 2 == 0 {
            "pipeline.worker.exit"
        } else {
            "pipeline.run.terminal_conflict"
        };
        for duplicate in 0..2 {
            seed_named_diagnostic(
                &runtime,
                &format!("exec-diagnostic-{index}-{duplicate}"),
                tool,
                &run_id,
                &task_id,
            );
        }
    }

    let body =
        body_json(request(runtime, "/audit/incidents?since=24h&class=diagnostic").await).await;

    assert_eq!(body["incident_count"], 7);
    assert_eq!(body["raw_failed_events"], 14);
    assert_eq!(body["matching_incident_count"], 7);
    assert_eq!(body["matching_raw_event_count"], 14);
    assert_eq!(body["matching_affected_run_count"], 7);
    assert_eq!(body["failure_categories"]["diagnostic"]["incidents"], 7);
    assert_eq!(body["failure_categories"]["diagnostic"]["raw_events"], 14);
    assert_eq!(body["failure_categories"]["diagnostic"]["affected_runs"], 7);
    assert_eq!(body["lifecycle_diagnostic_events"], 14);
    assert_eq!(body["lifecycle_diagnostic_incidents"], 7);
    assert_eq!(body["lifecycle_diagnostic_affected_run_count"], 7);

    let incidents = body["incidents"].as_array().expect("incidents");
    assert_eq!(incidents.len(), 7);
    for incident in incidents {
        assert_eq!(incident["class"], "diagnostic");
        assert_eq!(incident["event_count"], 2);
        let events = incident["events"].as_array().expect("exact evidence");
        assert_eq!(events.len(), 2);
        for event in events {
            assert!(event["id"].as_i64().is_some());
            assert!(event["run_id"].as_str().is_some());
            assert!(event["task_id"].as_str().is_some());
            assert!(event["tool"].as_str().is_some());
            assert!(event["execution_id"].as_str().is_some());
        }
    }
}
