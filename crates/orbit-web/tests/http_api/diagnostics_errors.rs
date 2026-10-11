use std::fs;

use chrono::{DateTime, Duration, Utc};
use orbit_cmd::DiagnosticsCommands;
use orbit_core::{JobRunState, V2AuditEventInsertParams};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

// Seed the production audit budget in one transaction, rather than paying for
// 50,000 runtime writes or adding a test-only setting to the HTTP server.
fn fill_stderr_audit_budget(fixture: &Fixture, count: usize, ts: DateTime<Utc>) {
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "WITH RECURSIVE filler(n) AS (
                         SELECT 1 UNION ALL SELECT n + 1 FROM filler WHERE n < ?1
                     )
                     INSERT INTO v2_audit_events
                       (workspace_id, event_id, source, schema_version, event_type,
                        ts, run_id, agent_identity, payload_json)
                     SELECT 'ws_http_fixture', 'filler-' || n, 'v2_envelope', 1,
                            'heartbeat', ?2, 'filler', 'http-fixture', '{}' FROM filler",
                    (i64::try_from(count).unwrap(), ts.to_rfc3339()),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

#[test]
fn stderr_audit_budget_discloses_truncation_and_distinguishes_exact_exhaustion() {
    isolated(
        "diagnostics_errors::stderr_audit_budget_discloses_truncation_and_distinguishes_exact_exhaustion",
        || {
            use orbit_common::storage::blob_store::BlobStore;

            // Event-budget exhaustion is independent of blob-budget exhaustion:
            // these cases read zero, one, or two blobs, far below the blob cap.
            for (truncated, with_recent_error) in [(true, false), (true, true), (false, true)] {
                let fixture = Fixture::new();
                write_process_rows(&fixture, &[]);
                let since: DateTime<Utc> = "2026-09-01T00:00:00Z".parse().unwrap();
                let boundary = since + Duration::days(2);
                let invoked_at = since + Duration::days(7);
                let blobs = BlobStore::new(fixture.runtime.data_root().join("state/audit/blobs"));
                let old_blob = blobs
                    .write(b"ERROR fixture: older excluded error\n")
                    .unwrap();
                insert(
                    &fixture,
                    "old-error",
                    "old-run",
                    since + Duration::days(1),
                    json!({"body_kind":"cli_invocation_finished", "stderr_blob_ref":old_blob}),
                );
                let mut seeded = 1;
                if truncated {
                    // This row is the oldest inside the budget. Its malformed
                    // payload must not move coverage to the next parsed event.
                    insert(
                        &fixture,
                        "boundary",
                        "boundary-run",
                        boundary,
                        json!({"body_kind":"heartbeat"}),
                    );
                    fixture.runtime.sqlite_store().unwrap().with_transaction(|tx| {
                        tx.connection().execute(
                            "UPDATE v2_audit_events SET payload_json = '[' WHERE event_id = 'boundary'",
                            [],
                        ).map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                        Ok(())
                    }).unwrap();
                    seeded += 1;
                }
                if with_recent_error {
                    let blob = blobs
                        .write(b"ERROR fixture: recent attributed error\n")
                        .unwrap();
                    for (id, days, body) in [
                        (
                            "step-a",
                            3,
                            json!({"body_kind":"step_started", "step_id":"A"}),
                        ),
                        (
                            "step-b",
                            4,
                            json!({"body_kind":"step_started", "step_id":"B"}),
                        ),
                        (
                            "bridge",
                            5,
                            json!({"body_kind":"activity_started", "parent_event_id":"step-b"}),
                        ),
                        (
                            "recent-error",
                            7,
                            json!({"body_kind":"cli_invocation_finished", "parent_event_id":"bridge", "stderr_blob_ref":blob}),
                        ),
                    ] {
                        insert(
                            &fixture,
                            id,
                            "recent-run",
                            since + Duration::days(days),
                            body,
                        );
                    }
                    seeded += 4;
                }
                fill_stderr_audit_budget(
                    &fixture,
                    50_000 + usize::from(truncated) - seeded,
                    boundary + Duration::hours(1),
                );
                let server = fixture.server(false);
                let response = json_ok(
                    server.get("/api/diagnostics/errors?month=2026-09&workspace=ws_http_fixture"),
                );
                assert_eq!(response["since"], since.to_rfc3339());
                assert_eq!(
                    response["coverage_since"],
                    if truncated { boundary } else { since }.to_rfc3339()
                );
                let rows = response["items"].as_array().unwrap();
                assert_eq!(
                    rows.len(),
                    usize::from(with_recent_error) + usize::from(!truncated)
                );
                assert_eq!(
                    rows.iter().any(|row| row["event_id"] == "old-error"),
                    !truncated
                );
                if with_recent_error {
                    let row = rows
                        .iter()
                        .find(|row| row["event_id"] == "recent-error")
                        .unwrap();
                    assert_eq!(row["message"], "recent attributed error");
                    assert_eq!(row["step"], "B");
                    assert_eq!(row["step_index"], 1);
                    assert_eq!(row["ts"], invoked_at.to_rfc3339());
                    // The raw scan reaches before this narrower window. Its
                    // ancestors still supply the original step index and join.
                    let response = json_ok(server.get(
                        "/api/diagnostics/errors?since=2026-09-07T00:00:00Z&workspace=ws_http_fixture",
                    ));
                    assert_eq!(response["coverage_since"], response["since"]);
                    assert_eq!(response["items"][0]["step"], "B");
                    assert_eq!(response["items"][0]["step_index"], 1);
                }
            }
        },
    );
}

#[test]
fn recent_diagnostics_skip_oversized_records_without_losing_neighbours() {
    isolated(
        "diagnostics_errors::recent_diagnostics_skip_oversized_records_without_losing_neighbours",
        || {
            let fixture = Fixture::new();
            let record = |step: &str, input: &str| {
                format!(
                    "{}\n",
                    json!({"ts":"2026-10-03T01:00:00Z", "job_run":"fixture", "step":step,
                        "command":"fixture", "input":input, "stderr":""})
                )
            };
            let bytes = [
                record("older", "small"),
                record("oversized", &"x".repeat(8 << 20)),
                record("newer", "small"),
            ]
            .concat();
            for category in ["metrics", "friction"] {
                let month = fixture
                    .runtime
                    .data_root()
                    .join("state/diagnostics")
                    .join(category)
                    .join("2026-10");
                fs::create_dir_all(&month).unwrap();
                fs::write(month.join("entries.jsonl"), &bytes).unwrap();
                let steps: Vec<_> = if category == "metrics" {
                    fixture
                        .runtime
                        .read_metrics_entries_limited("2026-10", 2)
                        .unwrap()
                        .into_iter()
                        .map(|entry| entry.step)
                        .collect()
                } else {
                    fixture
                        .runtime
                        .read_friction_entries_limited("2026-10", 2)
                        .unwrap()
                        .into_iter()
                        .map(|entry| entry.step)
                        .collect()
                };
                assert_eq!(steps, ["newer", "older"], "{category} recent records");
            }
        },
    );
}

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

/// The dashboard API's per-message cap for list projections.
const LIST_MESSAGE_MAX_BYTES: usize = 2048;

#[test]
fn long_step_errors_are_bounded_in_errors_and_friction_and_the_run_keeps_them() {
    isolated(
        "diagnostics_errors::long_step_errors_are_bounded_in_errors_and_friction_and_the_run_keeps_them",
        || {
            let fixture = Fixture::new();
            let start = Utc::now() - Duration::minutes(2);
            // A failed validation's message: a few hundred KB, with a bare
            // token straddling the cap that a cut before redaction would leak.
            let mut long = "required validation did not pass: ".to_string();
            long.push_str(&"x".repeat(LIST_MESSAGE_MAX_BYTES - 8 - long.len()));
            long.push_str(&format!(" ghp_{} ", "b".repeat(36)));
            long.push_str(&"y".repeat(300_000));
            long.push_str(" tail-marker");
            for (run, message) in [("long", long.as_str()), ("short", "boom")] {
                fixture.seed_run(run, "fixture", JobRunState::Failed);
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
                    start + Duration::seconds(2),
                    json!({"body_kind":"step_finished", "step_id":"X", "outcome":"failed", "error_message": message}),
                );
            }
            // A run-less row whose detail is in the process log itself.
            let mut unaffiliated =
                process(None, "unaffiliated-detail", start + Duration::seconds(3));
            unaffiliated["fields"]["error_message"] = json!(long);
            write_process_rows(
                &fixture,
                &[
                    process(Some("long"), "long", start + Duration::seconds(2)),
                    process(Some("short"), "short", start + Duration::seconds(2)),
                    unaffiliated,
                ],
            );
            let server = fixture.server(false);

            let errors = server
                .get("/api/diagnostics/errors?since=24h&workspace=ws_http_fixture")
                .text()
                .unwrap();
            assert!(!errors.contains("ghp_"), "redaction precedes the cut");
            let errors: Value = serde_json::from_str(&errors).unwrap();
            let rows = errors["items"].as_array().unwrap();
            let row = |id: &str| rows.iter().find(|row| row["event_id"] == id).unwrap();
            for id in ["long", "unaffiliated-detail"] {
                let message = row(id)["message"].as_str().unwrap();
                assert_eq!(message.len(), LIST_MESSAGE_MAX_BYTES, "{id}");
                assert!(long.starts_with(&message[..1024]), "{id} keeps the start");
                assert_eq!(row(id)["truncated"], true, "{id}");
            }
            assert_eq!(row("short")["message"], "boom");
            assert_eq!(row("short")["truncated"], false);

            let month = start.format("%Y-%m");
            let friction = server
                .get(&format!(
                    "/api/diagnostics/friction?month={month}&workspace=ws_http_fixture"
                ))
                .text()
                .unwrap();
            assert!(!friction.contains("ghp_"), "redaction precedes the cut");
            let friction: Value = serde_json::from_str(&friction).unwrap();
            let friction = friction.as_array().unwrap();
            let entry = |run: &str| friction.iter().find(|row| row["job_run"] == run).unwrap();
            assert_eq!(
                entry("long")["stderr"].as_str().unwrap().len(),
                LIST_MESSAGE_MAX_BYTES
            );
            assert_eq!(entry("long")["truncated"], true);
            assert_eq!(entry("short")["stderr"], "boom");
            assert_eq!(entry("short")["truncated"], false);

            // Both feeds link the row to its run, whose step keeps the full text.
            assert_eq!(row("long")["job_run"], "long");
            let run = json_ok(server.get("/api/runs/long?workspace=ws_http_fixture"));
            let step = run["steps"]
                .as_array()
                .unwrap()
                .iter()
                .find(|step| step["target_id"] == "X")
                .unwrap();
            assert_eq!(
                step["error_message"], long,
                "the run link reaches the full message"
            );
        },
    );
}

#[test]
fn historical_month_errors_share_inclusive_endpoints_across_sources() {
    isolated(
        "diagnostics_errors::historical_month_errors_share_inclusive_endpoints_across_sources",
        || {
            use orbit_common::storage::blob_store::BlobStore;

            let fixture = Fixture::new();
            let since: DateTime<Utc> = "2026-09-01T00:00:00Z".parse().unwrap();
            let until: DateTime<Utc> = "2026-09-30T23:59:59.999999999Z".parse().unwrap();
            let instants = [
                ("before", since - Duration::nanoseconds(1)),
                ("start", since),
                ("end", until),
                ("after", until + Duration::nanoseconds(1)),
            ];
            write_process_rows(&fixture, &instants.map(|(id, ts)| process(None, id, ts)));
            let blobs = BlobStore::new(fixture.runtime.data_root().join("state/audit/blobs"));
            // One invocation at the endpoint also carries lines outside the
            // month, exercising line filtering as well as invocation filtering.
            let stderr = instants
                .iter()
                .map(|(id, ts)| format!("{} ERROR fixture: {id}\n", ts.to_rfc3339()))
                .collect::<String>();
            let blob = blobs.write(stderr.as_bytes()).unwrap();
            for (id, ts) in instants {
                insert(
                    &fixture,
                    &format!("stderr-{id}"),
                    "fixture",
                    ts,
                    json!({"body_kind":"cli_invocation_finished", "stderr_blob_ref":blob}),
                );
            }
            let server = fixture.server(false);
            let response = json_ok(
                server.get("/api/diagnostics/errors?month=2026-09&workspace=ws_http_fixture"),
            );
            let rows = response["items"].as_array().unwrap();
            for source in ["process", "agent-stderr"] {
                let timestamps = rows
                    .iter()
                    .filter(|row| row["source"] == source)
                    .map(|row| {
                        row["ts"]
                            .as_str()
                            .unwrap()
                            .parse::<DateTime<Utc>>()
                            .unwrap()
                    })
                    .collect::<Vec<_>>();
                if source == "process" {
                    assert_eq!(timestamps, [until, since]);
                } else {
                    assert_eq!(timestamps, [until, until]);
                    for row in rows.iter().filter(|row| row["source"] == source) {
                        assert_eq!(row["message"], "start\nend");
                    }
                }
            }
            assert_eq!(response["since"], since.to_rfc3339());
            assert_eq!(response["coverage_since"], response["since"]);
        },
    );
}
