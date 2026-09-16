//! Endpoint tests for `GET /audit` (listing, paging, filters, error paths).
//!
//! ORB-10008: the audit surface previously had no handler-level coverage; these
//! tests drive the real router with seeded SQLite audit events.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use orbit_core::{AuditEventInsertParams, AuditEventStatus, OrbitRuntime};
use orbit_types::tool::{McpCapability, McpTransport};
use serde_json::Value;
use tower::ServiceExt;

use orbit_types::workflow::JobRunState;

use super::super::{HISTORY_MAX_LIMIT, router};
use super::test_support::{body_json, seed_run};

fn seed_audit_event(
    runtime: &OrbitRuntime,
    execution_id: &str,
    tool_name: &str,
    status: AuditEventStatus,
    role: &str,
    error_message: Option<&str>,
) {
    seed_audit_event_with_scope(
        runtime,
        execution_id,
        tool_name,
        status,
        role,
        error_message,
        Some(("jrun", None)),
    );
}

fn seed_audit_event_with_scope(
    runtime: &OrbitRuntime,
    execution_id: &str,
    tool_name: &str,
    status: AuditEventStatus,
    role: &str,
    error_message: Option<&str>,
    scope: Option<(&str, Option<&str>)>,
) {
    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: execution_id.to_string(),
            command: "tool".to_string(),
            subcommand: Some("run".to_string()),
            tool_name: Some(tool_name.to_string()),
            target_type: Some("task".to_string()),
            target_id: Some("T00000000-000000".to_string()),
            role: role.to_string(),
            status,
            exit_code: match status {
                AuditEventStatus::Success => 0,
                _ => 1,
            },
            duration_ms: 5,
            working_directory: "/tmp/fixture".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: error_message.map(str::to_string),
            host: None,
            pid: std::process::id(),
            session_id: None,
            workspace_id: Some("ws-orbit".to_string()),
            caller_machine_id: Some("hm-caller".to_string()),
            caller_host_id: Some("caller.local".to_string()),
            process_machine_id: Some("hm-process".to_string()),
            process_host_id: Some("process.local".to_string()),
            transport: Some(McpTransport::Local),
            effective_capabilities: [McpCapability::Agent, McpCapability::Runner]
                .into_iter()
                .collect(),
            origin_session_id: Some("mcp-session".to_string()),
            mcp_call_id: Some("mcall".to_string()),
            lease_id: Some("lease".to_string()),
            task_id: scope.and_then(|(_, task_id)| task_id).map(str::to_string),
            job_run_id: scope.map(|(run_id, _)| run_id.to_string()),
            activity_id: None,
            step_index: None,
        })
        .expect("seed audit event");
}

fn seed_fourteen_named_diagnostic_rows(runtime: &OrbitRuntime) {
    const RUN_IDS: [&str; 7] = [
        "jrun-diagnostic-0",
        "jrun-diagnostic-1",
        "jrun-diagnostic-2",
        "jrun-diagnostic-3",
        "jrun-diagnostic-4",
        "jrun-diagnostic-5",
        "jrun-diagnostic-6",
    ];
    for (index, run_id) in RUN_IDS.into_iter().enumerate() {
        let tool = if index % 2 == 0 {
            "pipeline.worker.exit"
        } else {
            "pipeline.run.terminal_conflict"
        };
        for duplicate in 0..2 {
            seed_audit_event_with_scope(
                runtime,
                &format!("exec-diagnostic-{index}-{duplicate}"),
                tool,
                AuditEventStatus::Failure,
                "admin",
                Some("pipeline worker lifecycle diagnostic"),
                Some((run_id, Some(&format!("REC-diagnostic-{index}")))),
            );
        }
    }
}

fn seed_lifecycle_audit_event(
    runtime: &OrbitRuntime,
    execution_id: &str,
    command: &str,
    job_run_id: Option<&str>,
    activity_id: Option<&str>,
    task_id: Option<&str>,
    error_message: &str,
) {
    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: execution_id.to_string(),
            command: command.to_string(),
            subcommand: (command != "Start").then(|| "run".to_string()),
            tool_name: None,
            target_type: Some("job_run".to_string()),
            target_id: job_run_id.map(str::to_string),
            role: "actor-one".to_string(),
            status: AuditEventStatus::Failure,
            exit_code: 1,
            duration_ms: 5,
            working_directory: "/tmp/fixture".to_string(),
            arguments_json: None,
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: Some(error_message.to_string()),
            host: None,
            pid: std::process::id(),
            session_id: None,
            workspace_id: Some("ws-orbit".to_string()),
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
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
    seed_lifecycle_audit_event(
        runtime,
        "exec-start-1",
        "Start",
        None,
        None,
        None,
        "job run start failed",
    );
    seed_lifecycle_audit_event(
        runtime,
        "exec-start-2",
        "Start",
        None,
        None,
        None,
        "job run start failed",
    );
    for index in 0..4 {
        let leaf = format!("jrun-leaf-{index}");
        let parent = format!("jrun-parent-{index}");
        seed_lifecycle_audit_event(
            runtime,
            &format!("exec-leaf-{index}"),
            "job",
            Some(&leaf),
            Some("leaf-step"),
            Some(&format!("REC-leaf-{index}")),
            "child step returned a nonzero status",
        );
        seed_lifecycle_audit_event(
            runtime,
            &format!("exec-parent-{index}"),
            "job",
            Some(&parent),
            Some("pipeline_success_guard"),
            Some(&format!("REC-parent-{index}")),
            &format!(
                "pipeline child run did not succeed: result run {leaf} status failed: child step returned a nonzero status"
            ),
        );
    }
}

async fn request_audit(runtime: OrbitRuntime, uri: &str) -> axum::response::Response {
    request_audit_on(crate::state::DashboardState::single(Arc::new(runtime)), uri).await
}

async fn request_audit_on(
    state: crate::state::DashboardState,
    uri: &str,
) -> axum::response::Response {
    router()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri(uri)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
}

fn execution_ids(rows: &[Value]) -> Vec<&str> {
    rows.iter()
        .map(|row| row["execution_id"].as_str().expect("execution_id"))
        .collect()
}

#[tokio::test]
async fn audit_lists_seeded_events_newest_first_with_projected_fields() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_audit_event(
        &runtime,
        "exec-1",
        "orbit.task.update",
        AuditEventStatus::Success,
        "editor",
        None,
    );
    seed_audit_event(
        &runtime,
        "exec-2",
        "orbit.task.add",
        AuditEventStatus::Failure,
        "editor",
        Some("boom"),
    );

    let response = request_audit(runtime, "/audit").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("audit array");
    assert_eq!(rows.len(), 2);
    // SQLite lists `ORDER BY id DESC`: the most recently inserted event first.
    assert_eq!(execution_ids(rows), vec!["exec-2", "exec-1"]);
    let newest = &rows[0];
    assert_eq!(newest["tool_name"], "orbit.task.add");
    assert_eq!(newest["status"], "failure");
    assert_eq!(newest["role"], "editor");
    assert_eq!(newest["error_message"], "boom");
    assert_eq!(newest["workspace_id"], "ws-orbit");
    assert_eq!(newest["caller_machine_id"], "hm-caller");
    assert_eq!(newest["caller_host_id"], "caller.local");
    assert_eq!(newest["process_machine_id"], "hm-process");
    assert_eq!(newest["process_host_id"], "process.local");
    assert_eq!(newest["transport"], "local");
    assert_eq!(
        newest["effective_capabilities"],
        serde_json::json!(["agent", "runner"])
    );
    assert_eq!(newest["origin_session_id"], "mcp-session");
    assert_eq!(newest["mcp_call_id"], "mcall");
    assert_eq!(newest["lease_id"], "lease");
    assert!(
        newest["timestamp"]
            .as_str()
            .is_some_and(|ts| { chrono::DateTime::parse_from_rfc3339(ts).is_ok() })
    );
}

#[tokio::test]
async fn audit_filters_all_trusted_mcp_provenance_fields() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_audit_event(
        &runtime,
        "exec-trusted",
        "orbit.task.list",
        AuditEventStatus::Success,
        "unverified",
        None,
    );

    let response = request_audit(
        runtime,
        concat!(
            "/audit?workspace_id=ws-orbit&caller_machine=hm-caller",
            "&process_machine=hm-process&transport=local&capability=runner",
            "&origin_session=mcp-session&mcp_call=mcall&job_run_id=jrun&lease=lease"
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("trusted provenance filtered rows");
    assert_eq!(execution_ids(rows), vec!["exec-trusted"]);
}

/// The page window is pushed into SQL, so paging is not capped at the first
/// `HISTORY_MAX_LIMIT` rows of history and a needle can reach rows older
/// than that window.
#[tokio::test]
async fn audit_paging_and_needle_reach_past_the_max_limit_window() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let total = HISTORY_MAX_LIMIT + 50;
    for index in 0..total {
        // The five oldest rows carry the only needle-matching tool name.
        let tool = if index < 5 {
            "orbit.needle.probe"
        } else {
            "orbit.task.update"
        };
        seed_audit_event(
            &runtime,
            &format!("exec-{index:04}"),
            tool,
            AuditEventStatus::Success,
            "editor",
            None,
        );
    }

    let response = request_audit(
        runtime.clone(),
        &format!("/audit?limit=50&offset={HISTORY_MAX_LIMIT}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("page past the old prefetch cap");
    assert_eq!(rows.len(), 50);
    assert_eq!(execution_ids(rows)[0], "exec-0049");

    let response = request_audit(runtime.clone(), "/audit?q=needle&limit=10").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("needle rows");
    assert_eq!(
        execution_ids(rows),
        vec![
            "exec-0004",
            "exec-0003",
            "exec-0002",
            "exec-0001",
            "exec-0000"
        ]
    );

    let response = request_audit(runtime, "/audit?q=needle&limit=2&offset=3").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("offset needle rows");
    assert_eq!(execution_ids(rows), vec!["exec-0001", "exec-0000"]);
}

#[tokio::test]
async fn audit_limit_and_offset_page_through_results_without_overlap() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    for index in 0..5 {
        seed_audit_event(
            &runtime,
            &format!("exec-{index}"),
            "orbit.task.update",
            AuditEventStatus::Success,
            "editor",
            None,
        );
    }

    let response = request_audit(runtime.clone(), "/audit?limit=3").await;
    assert_eq!(response.status(), StatusCode::OK);
    let first = body_json(response).await;
    let first_rows = first.as_array().expect("first page");
    assert_eq!(first_rows.len(), 3);

    let response = request_audit(runtime.clone(), "/audit?limit=3&offset=3").await;
    assert_eq!(response.status(), StatusCode::OK);
    let second = body_json(response).await;
    let second_rows = second.as_array().expect("second page");
    assert_eq!(second_rows.len(), 2);

    let mut all: Vec<String> = execution_ids(first_rows)
        .into_iter()
        .chain(execution_ids(second_rows))
        .map(str::to_string)
        .collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), 5, "pages must be disjoint and cover every event");

    // Offset past the end degrades to an empty page, not an error.
    let response = request_audit(runtime, "/audit?offset=50").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body, Value::Array(Vec::new()));
}

#[tokio::test]
async fn audit_filters_by_tool_status_run_id_alias_and_text_query() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_audit_event(
        &runtime,
        "exec-ok",
        "orbit.task.update",
        AuditEventStatus::Success,
        "editor",
        None,
    );
    seed_audit_event(
        &runtime,
        "exec-denied",
        "orbit.policy.check",
        AuditEventStatus::Denied,
        "admin",
        Some("blocked by policy needle"),
    );

    let response = request_audit(runtime.clone(), "/audit?tool=orbit.policy.check").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("tool-filtered array");
    assert_eq!(execution_ids(rows), vec!["exec-denied"]);

    let response = request_audit(runtime.clone(), "/audit?status=denied").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("status-filtered array");
    assert_eq!(execution_ids(rows), vec!["exec-denied"]);

    // `run_id` is the backward-compat alias of `execution_id` (T20260427-26).
    let response = request_audit(runtime.clone(), "/audit?run_id=exec-ok").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("run_id-filtered array");
    assert_eq!(execution_ids(rows), vec!["exec-ok"]);

    // When both are supplied, `execution_id` wins.
    let response = request_audit(
        runtime.clone(),
        "/audit?execution_id=exec-denied&run_id=exec-ok",
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("execution_id-precedence array");
    assert_eq!(execution_ids(rows), vec!["exec-denied"]);

    let response = request_audit(runtime, "/audit?q=needle").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    let rows = body.as_array().expect("text-filtered array");
    assert_eq!(execution_ids(rows), vec!["exec-denied"]);
}

#[tokio::test]
async fn audit_rejects_malformed_since_and_status_with_json_400() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request_audit(runtime.clone(), "/audit?since=not-a-time").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(body["error"].as_str().is_some_and(|m| !m.is_empty()));

    let response = request_audit(runtime, "/audit?status=bogus").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|m| m.contains("unknown audit event status"))
    );
}

#[tokio::test]
async fn audit_rejects_non_numeric_limit_without_500() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");

    let response = request_audit(runtime, "/audit?limit=lots").await;

    // Axum's Query extractor rejects before the handler runs: a clean client
    // error, never a 500/panic.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// ORB-10871: the header summary reports the raw failed-event count and the
/// grouped incident count as two distinct fields over the same window, so a
/// repeated burst can no longer be read as many independent failures.
#[tokio::test]
async fn audit_summary_separates_raw_failed_events_from_grouped_incidents() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    for index in 0..12 {
        seed_audit_event(
            &runtime,
            &format!("exec-burst-{index}"),
            "surface.alpha",
            AuditEventStatus::Failure,
            "actor-one",
            Some(&format!("could not remove /work/dir-{index}/file.txt")),
        );
    }
    seed_audit_event(
        &runtime,
        "exec-denied-one",
        "surface.beta",
        AuditEventStatus::Denied,
        "actor-one",
        Some("policy denied: write outside the allowed scope"),
    );

    let response = request_audit(runtime, "/audit/summary?since=24h").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["window"], "24h", "both counts state their window");
    assert_eq!(body["events"].as_u64(), Some(13), "all-events denominator");
    assert_eq!(
        body["failed_events"].as_u64(),
        Some(13),
        "raw failed rows stay counted in full"
    );
    assert_eq!(
        body["failure_incidents"].as_u64(),
        Some(2),
        "the burst collapses to one incident; the denial stays its own"
    );
    assert_eq!(
        body["failure_incidents_by_class"]["denied"].as_u64(),
        Some(1)
    );
    assert_eq!(
        body["failed_events_by_class"]["unexpected"].as_u64(),
        Some(12)
    );
}

/// ORB-10969: no-tool lifecycle rows must not appear as tool `unknown` in
/// failures-by-tool denominators or rates; they land in the named category.
#[tokio::test]
async fn audit_summary_excludes_lifecycle_rows_from_tool_metrics() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_ten_unknown_lifecycle_rows(&runtime);
    seed_audit_event(
        &runtime,
        "exec-tool-ok",
        "orbit.search",
        AuditEventStatus::Success,
        "actor-one",
        None,
    );
    seed_audit_event(
        &runtime,
        "exec-tool-fail",
        "orbit.search",
        AuditEventStatus::Failure,
        "actor-one",
        Some("search backend unavailable"),
    );

    let response = request_audit(runtime, "/audit/summary?since=24h").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["failed_events"].as_u64(), Some(11));
    assert_eq!(body["failure_incidents"].as_u64(), Some(6));
    assert_eq!(body["affected_run_count"].as_u64(), Some(9));
    assert_eq!(body["job_run_lifecycle_failures"].as_u64(), Some(10));
    assert_eq!(body["job_run_lifecycle_incidents"].as_u64(), Some(5));
    assert_eq!(body["job_run_lifecycle_label"], "job-run lifecycle");

    let failures = body["failures_by_tool"]
        .as_array()
        .expect("failures_by_tool");
    assert!(
        failures
            .iter()
            .all(|row| row["tool"] != "unknown" && row["tool"] != ""),
        "no-tool rows must not enter the tool histogram: {failures:?}"
    );
    let search = failures
        .iter()
        .find(|row| row["tool"] == "orbit.search")
        .expect("named tool still counted");
    assert_eq!(search["count"].as_u64(), Some(1));

    let rates = body["failure_rate_by_tool"]
        .as_array()
        .expect("failure_rate_by_tool");
    assert!(
        rates.iter().all(|row| row["tool"] != "unknown"),
        "lifecycle rows must not form a tool rate denominator"
    );
}

/// ORB-11118: rates compare only successful calls with unexpected failed
/// calls. Expected caller/input negatives, policy denials, and failure-only
/// lifecycle diagnostics keep their own raw/incident/run denominators.
#[tokio::test]
async fn audit_summary_separates_reliability_from_negative_and_diagnostic_populations() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_fourteen_named_diagnostic_rows(&runtime);

    for index in 0..5 {
        seed_audit_event_with_scope(
            &runtime,
            &format!("exec-search-ok-{index}"),
            "orbit.search",
            AuditEventStatus::Success,
            "actor-one",
            None,
            None,
        );
    }
    seed_audit_event_with_scope(
        &runtime,
        "exec-search-fail",
        "orbit.search",
        AuditEventStatus::Failure,
        "actor-one",
        Some("search backend unavailable"),
        None,
    );
    for (execution_id, tool, message) in [
        (
            "exec-show-negative",
            "orbit.task.show",
            "not found: task REC-missing",
        ),
        (
            "exec-update-negative",
            "orbit.task.update",
            "unsupported field projection: mystery",
        ),
    ] {
        seed_audit_event_with_scope(
            &runtime,
            execution_id,
            tool,
            AuditEventStatus::Failure,
            "actor-one",
            Some(message),
            None,
        );
    }
    seed_audit_event_with_scope(
        &runtime,
        "exec-denied",
        "orbit.task.update",
        AuditEventStatus::Denied,
        "actor-one",
        Some("policy denied: write outside allowed scope"),
        None,
    );

    let body = body_json(request_audit(runtime, "/audit/summary?since=24h").await).await;

    assert_eq!(body["failure_categories"]["unexpected"]["raw_events"], 1);
    assert_eq!(body["failure_categories"]["unexpected"]["incidents"], 1);
    assert_eq!(body["failure_categories"]["expected"]["raw_events"], 2);
    assert_eq!(body["failure_categories"]["denied"]["raw_events"], 1);
    assert_eq!(body["failure_categories"]["diagnostic"]["raw_events"], 14);
    assert_eq!(body["failure_categories"]["diagnostic"]["incidents"], 7);
    assert_eq!(body["failure_categories"]["diagnostic"]["affected_runs"], 7);
    assert_eq!(body["lifecycle_diagnostic_events"], 14);
    assert_eq!(body["lifecycle_diagnostic_incidents"], 7);
    assert_eq!(body["lifecycle_diagnostic_affected_run_count"], 7);

    let rates = body["failure_rate_by_tool"].as_array().expect("rates");
    assert_eq!(rates.len(), 1, "only one comparable callable surface");
    assert_eq!(rates[0]["tool"], "orbit.search");
    assert_eq!(rates[0]["failures"], 1);
    assert_eq!(rates[0]["successes"], 5);
    assert_eq!(rates[0]["total"], 6);
    assert_eq!(rates[0]["rate"].as_f64(), Some(1.0 / 6.0));
    assert!(rates.iter().all(|row| {
        row["tool"] != "pipeline.worker.exit"
            && row["tool"] != "pipeline.run.terminal_conflict"
            && row["tool"] != "orbit.task.show"
            && row["tool"] != "orbit.task.update"
    }));

    let unexpected = body["failures_by_tool"]
        .as_array()
        .expect("unexpected failures by tool");
    assert_eq!(unexpected.len(), 1);
    assert_eq!(unexpected[0]["tool"], "orbit.search");
    assert_eq!(unexpected[0]["count"], 1);
}

/// The header tiles count runs with `COUNT(*)`, so a bad day does not read
/// as exactly `HISTORY_MAX_LIMIT` failures once the real number passes it.
#[tokio::test]
async fn summary_failed_runs_counts_past_the_history_page_size() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    let total = HISTORY_MAX_LIMIT + 50;
    for index in 0..total {
        seed_run(
            &runtime,
            &format!("jrun-failed-{index:04}"),
            "task_pr_pipeline",
            JobRunState::Failed,
        );
    }
    seed_run(
        &runtime,
        "jrun-timeout",
        "task_pr_pipeline",
        JobRunState::Timeout,
    );
    seed_run(
        &runtime,
        "jrun-success",
        "task_pr_pipeline",
        JobRunState::Success,
    );

    let body = body_json(request_audit(runtime, "/audit/summary?since=24h").await).await;
    assert_eq!(body["failed_runs"].as_u64(), Some((total + 1) as u64));
}

/// Repeated polls on one server instance must not rescan the 24h window.
#[tokio::test]
async fn audit_summary_reuses_memo_for_repeated_polls_on_one_state() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_audit_event(
        &runtime,
        "exec-ok",
        "orbit.search",
        AuditEventStatus::Success,
        "actor-one",
        None,
    );
    let state = crate::state::DashboardState::single(Arc::new(runtime));

    let first = body_json(request_audit_on(state.clone(), "/audit/summary?since=24h").await).await;
    let second = body_json(request_audit_on(state.clone(), "/audit/summary?since=24h").await).await;

    assert_eq!(first["events"], second["events"]);
    assert_eq!(first["failed_events"], second["failed_events"]);
    assert_eq!(first["failure_incidents"], second["failure_incidents"]);
    assert_eq!(first["denials"], second["denials"]);
    assert_eq!(first["window"], "24h");
    assert_eq!(state.audit_summary_memo().compute_count(), 1);
}

/// Overlapping dashboard tabs share one compute rather than racing N scans.
#[tokio::test]
async fn audit_summary_coalesces_concurrent_polls_on_one_state() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_audit_event(
        &runtime,
        "exec-ok",
        "orbit.search",
        AuditEventStatus::Success,
        "actor-one",
        None,
    );
    let state = crate::state::DashboardState::single(Arc::new(runtime));

    let (left, right) = tokio::join!(
        request_audit_on(state.clone(), "/audit/summary?since=24h"),
        request_audit_on(state.clone(), "/audit/summary?since=24h"),
    );
    let left = body_json(left).await;
    let right = body_json(right).await;

    assert_eq!(left["events"], right["events"]);
    assert_eq!(left["failed_events"], right["failed_events"]);
    assert_eq!(state.audit_summary_memo().compute_count(), 1);
}

/// Relative windows are distinct keys; `24h` must not serve a `1h` payload.
#[tokio::test]
async fn audit_summary_does_not_share_memo_across_windows() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_audit_event(
        &runtime,
        "exec-ok",
        "orbit.search",
        AuditEventStatus::Success,
        "actor-one",
        None,
    );
    let state = crate::state::DashboardState::single(Arc::new(runtime));

    let day = body_json(request_audit_on(state.clone(), "/audit/summary?since=24h").await).await;
    let hour = body_json(request_audit_on(state.clone(), "/audit/summary?since=1h").await).await;

    assert_eq!(day["window"], "24h");
    assert_eq!(hour["window"], "1h");
    assert_eq!(state.audit_summary_memo().compute_count(), 2);
}

/// `denial_threshold` is request echo, not part of the scan, so a memo hit
/// still reflects the caller's threshold.
#[tokio::test]
async fn audit_summary_applies_denial_threshold_after_memo_hit() {
    let runtime = OrbitRuntime::in_memory().expect("build runtime");
    seed_audit_event(
        &runtime,
        "exec-ok",
        "orbit.search",
        AuditEventStatus::Success,
        "actor-one",
        None,
    );
    let state = crate::state::DashboardState::single(Arc::new(runtime));

    let first = body_json(request_audit_on(state.clone(), "/audit/summary?since=24h").await).await;
    let second = body_json(
        request_audit_on(state.clone(), "/audit/summary?since=24h&denial_threshold=3").await,
    )
    .await;

    assert_eq!(first["denial_threshold"].as_i64(), Some(10));
    assert_eq!(second["denial_threshold"].as_i64(), Some(3));
    assert_eq!(first["events"], second["events"]);
    assert_eq!(state.audit_summary_memo().compute_count(), 1);
}
