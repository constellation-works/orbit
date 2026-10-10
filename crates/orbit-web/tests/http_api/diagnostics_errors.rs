use std::fs;

use chrono::{DateTime, Duration, Utc};
use orbit_core::{JobRunState, V2AuditEventInsertParams};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

fn insert(fixture: &Fixture, id: &str, run: &str, ts: DateTime<Utc>, mut body: Value) {
    body["event_id"] = json!(id);
    body["ts"] = json!(ts.to_rfc3339());
    body["run_id"] = json!(run);
    fixture
        .runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: fixture.runtime.workspace_id().unwrap(),
            event_id: id.into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: body["body_kind"].as_str().unwrap().into(),
            ts,
            run_id: run.into(),
            agent_identity: "http-fixture".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: body.to_string(),
        })
        .unwrap();
}

fn process(run: Option<&str>, id: &str, ts: DateTime<Utc>) -> Value {
    json!({
        "timestamp": ts.to_rfc3339(), "level": "ERROR", "target": "orbit.job.step_finished",
        "fields": {"job_run_id": run, "step_id": "X", "outcome": "failed", "success": false, "event_id": id},
    })
}

fn write_process_rows(fixture: &Fixture, rows: &[Value]) {
    fs::write(
        fixture.path("process.log"),
        rows.iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
}

#[test]
fn resumed_step_errors_keep_each_attempts_reason_and_mark_run_recovery() {
    isolated(
        "diagnostics_errors::resumed_step_errors_keep_each_attempts_reason_and_mark_run_recovery",
        || {
            let fixture = Fixture::new();
            fixture.seed_run("resumed", "fixture", JobRunState::Success);
            fixture.seed_run("still-failed", "fixture", JobRunState::Failed);
            let start = Utc::now() - Duration::minutes(2);
            // The first process event precedes its audit completion, as the writer
            // does in production. The second follows it, covering imported logs.
            for run in ["resumed", "still-failed"] {
                for (suffix, seconds, body) in [
                    (
                        "start",
                        0,
                        json!({"body_kind":"step_started", "step_id":"X"}),
                    ),
                    (
                        "fail",
                        2,
                        json!({"body_kind":"step_finished", "step_id":"X", "outcome":"failed", "error_message":"boom"}),
                    ),
                    (
                        "restart",
                        3,
                        json!({"body_kind":"step_started", "step_id":"X"}),
                    ),
                    (
                        "error",
                        5,
                        json!({"body_kind":"step_finished", "step_id":"X", "outcome":"error", "error_message":"second failure"}),
                    ),
                    (
                        "resume",
                        6,
                        json!({"body_kind":"step_started", "step_id":"X"}),
                    ),
                    (
                        "success",
                        8,
                        json!({"body_kind":"step_finished", "step_id":"X", "outcome":"success"}),
                    ),
                ] {
                    insert(
                        &fixture,
                        &format!("{run}-{suffix}"),
                        run,
                        start + Duration::seconds(seconds),
                        body,
                    );
                }
                let final_steps = fixture.runtime.collect_run_audit_steps(run).unwrap();
                assert_eq!(final_steps.len(), 1);
                assert_eq!(final_steps[0].outcome.as_deref(), Some("success"));
                assert_eq!(final_steps[0].error_message, None);
            }
            write_process_rows(
                &fixture,
                &[
                    process(
                        Some("resumed"),
                        "first",
                        start + Duration::seconds(2) - Duration::milliseconds(1),
                    ),
                    process(
                        Some("resumed"),
                        "second",
                        start + Duration::seconds(5) + Duration::milliseconds(1),
                    ),
                    process(
                        Some("still-failed"),
                        "unrecovered",
                        start + Duration::seconds(2) - Duration::milliseconds(1),
                    ),
                ],
            );
            let server = fixture.server(false);
            let response =
                json_ok(server.get("/api/diagnostics/errors?since=24h&workspace=ws_http_fixture"));
            let rows = response["items"].as_array().unwrap();
            assert_eq!(rows.len(), 3);
            let row = |id: &str| rows.iter().find(|row| row["event_id"] == id).unwrap();
            assert_eq!(row("first")["message"], "boom");
            assert_eq!(row("second")["message"], "second failure");
            assert_eq!(row("first")["step_index"], 0);
            assert_eq!(row("second")["step_index"], 0);
            assert_eq!(row("first")["recovered"], true);
            assert_eq!(row("second")["recovered"], true);
            assert_eq!(row("unrecovered")["message"], "boom");
            assert_eq!(
                row("unrecovered")["recovered"],
                false,
                "a successful step alone does not mean the run recovered"
            );
        },
    );
}

#[test]
fn step_errors_without_failure_detail_report_its_absence() {
    isolated(
        "diagnostics_errors::step_errors_without_failure_detail_report_its_absence",
        || {
            let fixture = Fixture::new();
            let start = Utc::now() - Duration::minutes(2);
            for run in [
                "missing-message",
                "blank-message",
                "missing-audit",
                "direct-detail",
            ] {
                fixture.seed_run(run, "fixture", JobRunState::Failed);
            }
            for (run, message) in [
                ("missing-message", Value::Null),
                ("blank-message", json!("  \n")),
            ] {
                insert(
                    &fixture,
                    &format!("{run}-start"),
                    run,
                    start,
                    json!({"body_kind":"step_started", "step_id":"X"}),
                );
                insert(
                    &fixture,
                    &format!("{run}-finish"),
                    run,
                    start + Duration::seconds(1),
                    json!({"body_kind":"step_finished", "step_id":"X", "outcome":"failed", "error_message": message}),
                );
            }
            let mut direct = process(
                Some("direct-detail"),
                "direct-detail",
                start + Duration::seconds(1),
            );
            direct["fields"]["error_message"] = json!("recorded in process log");
            write_process_rows(
                &fixture,
                &[
                    process(
                        Some("missing-message"),
                        "missing-message",
                        start + Duration::seconds(1),
                    ),
                    process(
                        Some("blank-message"),
                        "blank-message",
                        start + Duration::seconds(1),
                    ),
                    process(
                        Some("missing-audit"),
                        "missing-audit",
                        start + Duration::seconds(1),
                    ),
                    process(None, "unaffiliated", start + Duration::seconds(1)),
                    direct,
                ],
            );
            let server = fixture.server(false);
            let response =
                json_ok(server.get("/api/diagnostics/errors?since=24h&workspace=ws_http_fixture"));
            let rows = response["items"].as_array().unwrap();
            assert_eq!(rows.len(), 5);
            for row in rows {
                if row["event_id"] == "direct-detail" {
                    assert_eq!(row["message"], "recorded in process log");
                } else {
                    assert_eq!(row["message"], "no failure detail recorded");
                    assert!(!row["message"].as_str().unwrap().contains("step X finished"));
                }
            }
        },
    );
}
