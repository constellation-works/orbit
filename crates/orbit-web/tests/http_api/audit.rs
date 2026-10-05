//! Audit counts through the HTTP API and real runtime dispatch, in isolated children.

use chrono::{DateTime, Duration, Utc};
use orbit_core::{
    AuditEventInsertParams, AuditEventStatus, FailureClass, FailureIncidentQuery, JobRunState,
    OrbitError,
};
use serde_json::json;

use super::support::{Fixture, isolated, json_ok};

#[test]
fn failed_runs_tile_matches_filtered_terminal_runs() {
    isolated(
        "audit::failed_runs_tile_matches_filtered_terminal_runs",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            for (index, (id, state)) in [
                ("success", JobRunState::Success),
                ("failed", JobRunState::Failed),
                ("timeout", JobRunState::Timeout),
                ("cancelled", JobRunState::Cancelled),
                ("interrupted", JobRunState::Interrupted),
            ]
            .into_iter()
            .enumerate()
            {
                let mut run = fixture.seed_run(id, "terminal", state);
                let timestamp = now - Duration::seconds(5 - index as i64);
                run.created_at = timestamp;
                run.started_at = Some(timestamp);
                run.finished_at = Some(timestamp);
                fixture.save_run(&run);
            }
            // A known job with no runs, so the job filter is an empty match
            // rather than an unknown-job refusal.
            fixture.job("other");

            let server = fixture.server(false);
            let summary =
                json_ok(server.get("/api/audit/summary?since=24h&workspace=ws_http_fixture"));
            let filtered = json_ok(
                server
                    .request("GET", "/api/job-runs")
                    .query(&[
                        ("workspace", "ws_http_fixture"),
                        ("state", "failed"),
                        ("since", summary["since"].as_str().unwrap()),
                    ])
                    .send()
                    .unwrap(),
            );
            assert_eq!(summary["failed_runs"], 3);
            assert_eq!(summary["failed_runs"], filtered["total"]);
            let ids = |page: &serde_json::Value| {
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|run| run["run_id"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            };
            assert_eq!(ids(&filtered), ["interrupted", "timeout", "failed"]);
            assert_eq!(filtered["truncated"], false);
            let bounded = json_ok(server.get(
                "/api/job-runs?state=failed&limit=2&job_id=terminal&workspace=ws_http_fixture",
            ));
            assert_eq!(bounded["total"], 3);
            assert_eq!(bounded["truncated"], true);
            assert_eq!(ids(&bounded), ["interrupted", "timeout"]);
            let aggregate = json_ok(server.get("/api/job-runs/all?state=failed&limit=2"));
            assert_eq!(ids(&aggregate), ids(&bounded));
            let all_failures = json_ok(server.get("/api/job-runs/all?state=failed"));
            assert_eq!(ids(&all_failures), ids(&filtered));
            let unrelated = json_ok(
                server.get("/api/job-runs?state=failed&job_id=other&workspace=ws_http_fixture"),
            );
            assert_eq!(unrelated["total"], 0);
            assert!(ids(&unrelated).is_empty());
            let future = (now + Duration::hours(1)).to_rfc3339();
            let future_page = json_ok(
                server
                    .request(
                        "GET",
                        "/api/job-runs?state=failed&workspace=ws_http_fixture",
                    )
                    .query(&[("since", future)])
                    .send()
                    .unwrap(),
            );
            assert_eq!(future_page["total"], 0);
            assert!(ids(&future_page).is_empty());
        },
    );
}

fn row(id: &str, tool: Option<&str>, status: AuditEventStatus) -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: id.into(),
        command: "tool".into(),
        subcommand: Some("run-mcp".into()),
        tool_name: tool.map(str::to_string),
        target_type: None,
        target_id: None,
        role: "codex".into(),
        status,
        exit_code: i32::from(status != AuditEventStatus::Success),
        duration_ms: 1,
        working_directory: ".".into(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: None,
        pid: 1,
        session_id: None,
        workspace_id: Some("ws_http_fixture".into()),
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
        job_run_id: None,
        activity_id: None,
        step_index: None,
    }
}

#[test]
fn callable_failures_reconcile_raw_classified_and_denied_rows_with_events() {
    isolated(
        "audit::callable_failures_reconcile_raw_classified_and_denied_rows_with_events",
        || {
            use AuditEventStatus::{Denied, Failure, Success};
            const TOOL: &str = "orbit.workflow.run.list";
            let fixture = Fixture::new();
            for index in 0..12 {
                let status = match index {
                    0..=6 => Success,
                    7..=8 => Failure,
                    _ => Denied,
                };
                let mut event = row(&format!("mixed-{index}"), Some(TOOL), status);
                event.subcommand = Some(if index % 2 == 0 { "run" } else { "run-mcp" }.into());
                event.error_message = match index {
                    7 => Some("execution failed: Invalid input: unknown field".into()),
                    8 => Some("I/O error: disk unavailable".into()),
                    9..=11 => Some("operator capability required".into()),
                    _ => None,
                };
                // Workspace routing is deliberately distinct from the stored-ID filter:
                // both summary and Events read host-global history unless explicitly filtered.
                if index == 0 {
                    event.workspace_id = Some("ws_other".into());
                }
                fixture.runtime.record_audit_event(&event).unwrap();
            }
            for (id, tool, status, message) in [
                ("denied-only", Some("orbit.friction.list"), Denied, None),
                ("success-only", Some("orbit.task.list"), Success, None),
                (
                    "expected-only",
                    Some("orbit.task.show"),
                    Failure,
                    Some("not found: task"),
                ),
                ("empty-message", Some("github.run.list"), Failure, None),
                (
                    "diagnostic",
                    Some("pipeline.worker.exit"),
                    Failure,
                    Some("worker exited"),
                ),
                ("unnamed", None, Failure, None),
                ("blank-name", Some(" "), Failure, None),
                ("old", Some(TOOL), Failure, Some("old I/O error")),
            ] {
                let mut event = row(id, tool, status);
                event.error_message = message.map(str::to_string);
                fixture.runtime.record_audit_event(&event).unwrap();
            }
            for (id, command, subcommand) in [
                ("authorization", "authorization", "run-mcp"),
                ("not-a-call", "tool", "show"),
            ] {
                let mut event = row(id, Some(TOOL), Failure);
                event.command = command.into();
                event.subcommand = Some(subcommand.into());
                fixture.runtime.record_audit_event(&event).unwrap();
            }
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    tx.connection()
                        .execute(
                            "UPDATE audit_events SET timestamp = ?1 WHERE execution_id = 'old'",
                            [(Utc::now() - Duration::days(2)).to_rfc3339()],
                        )
                        .map_err(|error| OrbitError::Store(error.to_string()))?;
                    Ok(())
                })
                .unwrap();

            let server = fixture.server(false);
            let summary =
                json_ok(server.get("/api/audit/summary?since=24h&workspace=ws_http_fixture"));
            let rows = summary["tool_call_failures_by_tool"].as_array().unwrap();
            let mixed = rows.iter().find(|item| item["tool"] == TOOL).unwrap();
            assert_eq!(mixed["failed"], 2);
            assert_eq!(
                mixed["total"], 9,
                "denied calls must not dilute the raw failure rate"
            );
            assert_eq!(mixed["rate"], json!(2.0 / 9.0));
            assert_eq!(mixed["unexpected"], 1);
            assert_eq!(mixed["denied"], 3);
            assert_eq!(
                rows.len(),
                4,
                "success-only, unnamed and diagnostic tools do not enter the ranking"
            );
            assert_eq!(
                rows.iter()
                    .find(|item| item["tool"] == "orbit.friction.list")
                    .unwrap(),
                &json!({"tool":"orbit.friction.list","failed":0,"total":0,"rate":0.0,"unexpected":0,"denied":1})
            );
            assert_eq!(
                rows.iter()
                    .find(|item| item["tool"] == "orbit.task.show")
                    .unwrap()["unexpected"],
                0
            );
            assert_eq!(
                rows.iter()
                    .find(|item| item["tool"] == "github.run.list")
                    .unwrap()["unexpected"],
                1,
                "empty messages retain conservative classification"
            );
            let rate = &summary["tool_call_failure_rate"];
            assert_eq!(rate["failed"], 4);
            assert_eq!(
                rate["total"], 12,
                "success-only tools remain in the overall population"
            );
            assert_eq!(rate["unexpected"], 2);
            assert_eq!(rate["denied"], 4);
            assert_eq!(rate["rate"], json!(4.0 / 12.0));

            let since = DateTime::parse_from_rfc3339(summary["since"].as_str().unwrap())
                .unwrap()
                .with_timezone(&Utc);
            let incidents = fixture
                .runtime
                .audit_failure_incidents(&FailureIncidentQuery {
                    since: Some(since),
                    ..Default::default()
                })
                .unwrap();
            for counts in rows {
                let tool = counts["tool"].as_str().unwrap();
                let events = json_ok(
                    server
                        .request("GET", "/api/audit")
                        .query(&[
                            ("workspace", "ws_http_fixture"),
                            ("since", summary["since"].as_str().unwrap()),
                            ("tool", tool),
                            ("limit", "100"),
                        ])
                        .send()
                        .unwrap(),
                );
                let calls: Vec<_> = events
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|event| {
                        event["command"] == "tool"
                            && matches!(event["subcommand"].as_str(), Some("run" | "run-mcp"))
                    })
                    .collect();
                let count = |status| {
                    calls
                        .iter()
                        .filter(|event| event["status"] == status)
                        .count() as u64
                };
                assert_eq!(
                    counts["failed"],
                    count("failure"),
                    "raw Events failures for {tool}"
                );
                assert_eq!(
                    counts["total"],
                    count("success") + count("failure"),
                    "comparable Events calls for {tool}"
                );
                assert_eq!(
                    counts["denied"],
                    count("denied"),
                    "Events denials for {tool}"
                );
                let unexpected = calls
                    .iter()
                    .filter(|event| {
                        incidents.incidents.iter().any(|incident| {
                            incident.class == FailureClass::Unexpected
                                && incident
                                    .events
                                    .iter()
                                    .any(|reference| event["id"] == reference.id)
                        })
                    })
                    .count() as u64;
                assert_eq!(
                    counts["unexpected"], unexpected,
                    "shared incident classifier for {tool}"
                );
            }
        },
    );
}

#[test]
fn inactive_tool_calls_are_denied_by_registry_and_audit_classifier() {
    isolated(
        "audit::inactive_tool_calls_are_denied_by_registry_and_audit_classifier",
        || {
            let fixture = Fixture::new();
            let error = fixture
                .runtime
                .execute_tool_command("orbit.friction.list", json!({"model":"codex"}), None, None)
                .unwrap_err();
            assert!(
                matches!(error, OrbitError::PolicyDenied(_)),
                "inactive tool is a policy refusal: {error}"
            );
            // The second registry refusal site is the already-set inactive toggle.
            let error = fixture
                .runtime
                .enable_tool("orbit.friction.list")
                .unwrap_err();
            assert!(
                matches!(error, OrbitError::PolicyDenied(_)),
                "inactive no-op toggle is a policy refusal: {error}"
            );
            let events = fixture
                .runtime
                .list_audit_events(None, Some("orbit.friction.list".into()), None, None, 100)
                .unwrap();
            assert_eq!(
                events.len(),
                1,
                "one runtime tool attempt produces one audit row"
            );
            assert_eq!(events[0].status, AuditEventStatus::Denied);
            let report = fixture
                .runtime
                .audit_failure_incidents(&FailureIncidentQuery::default())
                .unwrap();
            assert_eq!(report.incidents.len(), 1);
            assert_eq!(report.incidents[0].class, FailureClass::Denied);
            assert_eq!(report.raw_events_by_class["denied"], 1);
        },
    );
}

#[test]
fn dashboard_renders_callable_failure_counts_and_denial_only_tools() {
    let result = std::process::Command::new("node")
        .args([
            "--experimental-vm-modules",
            "tests/http_api/dashboard_audit.mjs",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("node is required for dashboard behavior tests");
    assert!(
        result.status.success(),
        "dashboard audit behavior failed:\n{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
