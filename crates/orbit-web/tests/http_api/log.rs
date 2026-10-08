use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;

use orbit_core::JobRunState;
use reqwest::blocking::Response;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

fn line(step: &str) -> String {
    format!(
        "{}\n",
        json!({
            "timestamp":"2026-10-03T01:00:00Z", "level":"INFO", "target":"orbit.job.step_started",
            "fields":{"job_run_id":"http-log-fixture","step_id":step},
        })
    )
}

fn append(path: &Path, text: &str) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
    file.flush().unwrap();
}

fn stream(response: Response) -> BufReader<Response> {
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    BufReader::new(response)
}

/// The HTTP client's total request timeout bounds even a missing SSE delimiter/event.
fn event(reader: &mut impl BufRead) -> (u64, Value) {
    let (id, value) = feed_event(reader);
    (id.parse().unwrap(), value)
}

fn feed_event(reader: &mut impl BufRead) -> (String, Value) {
    let mut id = None;
    let mut data = None;
    loop {
        let mut line = String::new();
        assert!(
            reader
                .read_line(&mut line)
                .expect("SSE event within HTTP timeout")
                > 0,
            "SSE stream ended before an event"
        );
        let line = line.trim_end();
        if let Some(value) = line.strip_prefix("id:") {
            id = Some(value.trim().to_owned());
        }
        if let Some(value) = line.strip_prefix("data:") {
            data = Some(serde_json::from_str(value.trim()).unwrap());
        }
        if line.is_empty()
            && let Some(data) = data.take()
        {
            return (id.expect("SSE byte cursor"), data);
        }
    }
}

fn assert_step(value: &Value, step: &str) {
    assert!(
        value["message_html"].as_str().unwrap().contains(step),
        "expected log event {step}: {value}"
    );
}

/// Exercises the real snapshot/SSE formatter and shipped dashboard in Chromium.
/// Browser dependencies and evidence paths are supplied explicitly; ordinary
/// test runs do not download a browser or silently skip its assertions.
#[test]
#[ignore = "requires ORBIT_PLAYWRIGHT_MODULE and ORBIT_LOG_BROWSER_EVIDENCE_DIR"]
fn dashboard_log_message_priority_and_agent_filter() {
    isolated(
        "log::dashboard_log_message_priority_and_agent_filter",
        || {
            let fixture = Fixture::new();
            let log = fixture.path("process.log");
            let relay = |stream: &str, cwd: &Path, line: &str| {
                json!({
                    "timestamp": "2026-10-07T01:00:00Z",
                    "level": if stream == "stderr" { "ERROR" } else { "INFO" },
                    "target": "orbit_engine::activity_job::cli_runner::supervisor",
                    "fields": {
                        "cwd": cwd, "job_run_id": "jrun-20261007-0722-c12",
                        "provider": "codex", "stream": stream, "line": line,
                    },
                })
            };
            fs::write(&log, line("dispatch")).unwrap();
            append(
                &log,
                &format!(
                    "{}\n",
                    relay(
                        "stderr",
                        &fixture.path("workspace/project"),
                        "agent diagnostic"
                    )
                ),
            );
            append(
                &log,
                &format!(
                    "{}\n",
                    relay(
                        "stdout",
                        &fixture
                            .path("repo/.orbit/state/worktrees/orbit-jrun-20261007-0722-c12/src"),
                        r#"{"type":"item.started","item":{"type":"command_execution"}}"#,
                    )
                ),
            );
            let server = fixture.server(false);
            let output = Command::new("node")
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/http_api/log_browser.mjs"))
                .arg(
                    std::env::var_os("ORBIT_PLAYWRIGHT_MODULE")
                        .expect("prepared Playwright module"),
                )
                .arg(
                    std::env::var_os("ORBIT_LOG_BROWSER_EVIDENCE_DIR")
                        .expect("browser evidence directory"),
                )
                .arg(&server.origin)
                .arg(&log)
                .output()
                .expect("launch dashboard log browser fixture");
            assert!(
                output.status.success(),
                "dashboard log browser check failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        },
    );
}

#[test]
fn snapshots_and_diagnostics_skip_non_utf8_log_records() {
    isolated(
        "log::snapshots_and_diagnostics_skip_non_utf8_log_records",
        || {
            let fixture = Fixture::new();
            fixture.seed_run("http-log-fixture", "fixture", JobRunState::Failed);
            let record = |timestamp: &str, level: &str, step: &str| {
                format!(
                    "{}\n",
                    json!({
                        "timestamp": timestamp, "level": level, "target": "orbit.job.step_started",
                        "fields": {"job_run_id": "http-log-fixture", "step_id": step},
                    })
                )
            };
            let mut bytes = record("2026-10-03T01:00:00Z", "ERROR", "older-error").into_bytes();
            bytes.extend_from_slice(b"malformed JSON\n");
            // A truncated multibyte character inside an otherwise valid JSON record.
            bytes.extend_from_slice(
                b"{\"level\":\"ERROR\",\"fields\":{\"message\":\"torn-\xe2\x82\"}}\n",
            );
            bytes.extend_from_slice(record("2026-10-03T01:01:00Z", "INFO", "info").as_bytes());
            bytes.extend_from_slice(
                record("2026-10-03T01:02:00Z", "ERROR", "newer-error").as_bytes(),
            );
            fs::write(fixture.path("process.log"), &bytes).unwrap();
            let server = fixture.server(false);

            let response = server.get("/api/log?limit=3");
            assert_eq!(response.status().as_u16(), 200);
            let snapshot = json_ok(response);
            assert_eq!(snapshot["events"].as_array().unwrap().len(), 3);
            for (event, step) in snapshot["events"].as_array().unwrap().iter().zip([
                "older-error",
                "info",
                "newer-error",
            ]) {
                assert_step(event, step);
            }
            assert_eq!(snapshot["offset"], bytes.len() as u64);

            let response = server.get("/api/diagnostics/errors?limit=2");
            assert_eq!(response.status().as_u16(), 200);
            let errors = json_ok(response)["items"].clone();
            assert_eq!(errors.as_array().unwrap().len(), 2);
            assert_eq!(errors[0]["step"], "newer-error");
            assert_eq!(errors[1]["step"], "older-error");
            for error in errors.as_array().unwrap() {
                assert_eq!(error["source"], "process");
                assert_eq!(error["job_run"], "http-log-fixture");
            }
        },
    );
}

#[test]
fn sse_replays_snapshot_gap_and_last_event_id_without_duplicates() {
    isolated(
        "log::sse_replays_snapshot_gap_and_last_event_id_without_duplicates",
        || {
            let fixture = Fixture::new();
            let path = fixture.path("process.log");
            let first = line("initial");
            fs::write(&path, &first).unwrap();
            let server = fixture.server(false);
            let snapshot = json_ok(server.get("/api/log?target=orbit.job&limit=20"));
            assert_eq!(snapshot["events"].as_array().unwrap().len(), 1);
            assert_step(&snapshot["events"][0], "initial");
            assert_eq!(snapshot["offset"], first.len() as u64);
            let second = line("snapshot-gap");
            append(&path, &second);
            let mut reader = stream(server.get(&format!(
                "/api/log/stream?target=orbit.job&from={}",
                snapshot["offset"]
            )));
            let (cursor, received) = event(&mut reader);
            assert_step(&received, "snapshot-gap");
            assert_eq!(cursor, (first.len() + second.len()) as u64);
            append(&path, &line("after-open"));
            let (cursor, received) = event(&mut reader);
            assert_step(&received, "after-open");
            drop(reader);

            append(&path, &line("reconnect-only"));
            let mut resumed = stream(
                server
                    .request("GET", "/api/log/stream?target=orbit.job&from=0")
                    .header("last-event-id", cursor.to_string())
                    .send()
                    .unwrap(),
            );
            let (cursor, received) = event(&mut resumed);
            assert_step(&received, "reconnect-only");
            append(&path, &line("sentinel"));
            let (next, received) = event(&mut resumed);
            assert_step(&received, "sentinel");
            assert!(
                next > cursor,
                "SSE cursors advance past each complete record"
            );
        },
    );
}

#[test]
fn sse_restarts_after_rotation_with_live_and_stale_cursors() {
    isolated(
        "log::sse_restarts_after_rotation_with_live_and_stale_cursors",
        || {
            let fixture = Fixture::new();
            let path = fixture.path("process.log");
            fs::write(
                &path,
                format!("{}{}{}", line("old-1"), line("old-2"), line("old-3")),
            )
            .unwrap();
            let server = fixture.server(false);
            let mut live = stream(server.get("/api/log/stream?from=0"));
            let mut old_cursor = 0;
            for step in ["old-1", "old-2", "old-3"] {
                let (cursor, received) = event(&mut live);
                assert_step(&received, step);
                old_cursor = cursor;
            }
            // A rename models rotation by the logger rather than merely appending.
            fs::rename(&path, fixture.path("process.log.1")).unwrap();
            fs::write(&path, line("rotated")).unwrap();
            let (cursor, received) = event(&mut live);
            assert_step(&received, "rotated");
            assert!(cursor < old_cursor, "rotation resets the byte cursor");
            drop(live);

            let mut reconnect = stream(
                server
                    .request("GET", "/api/log/stream?from=0")
                    .header("last-event-id", old_cursor.to_string())
                    .send()
                    .unwrap(),
            );
            let (_, received) = event(&mut reconnect);
            assert_step(&received, "rotated");
            append(&path, &line("rotation-sentinel"));
            let (_, received) = event(&mut reconnect);
            assert_step(&received, "rotation-sentinel");
        },
    );
}

#[test]
fn split_log_snapshot_stream_and_reconnect_preserve_agent_output() {
    isolated(
        "log::split_log_snapshot_stream_and_reconnect_preserve_agent_output",
        || {
            let fixture = Fixture::new();
            let path = fixture.path("orbit.jsonl");
            let agent = fixture.path("orbit-agent.jsonl");
            let relay = |text: &str| {
                format!(
                    "{}\n",
                    json!({
                        "timestamp":"2026-10-07T01:00:00Z", "level":"INFO", "target":"orbit_engine::activity_job::cli_runner::supervisor",
                        "fields":{"provider":"codex", "stream":"stdout", "line":text},
                    })
                )
            };
            fs::write(&path, line("operational")).unwrap();
            fs::write(&agent, relay("initial-agent")).unwrap();
            let server = fixture.split_log_server();
            let snapshot = json_ok(server.get("/api/log?limit=20"));
            let events = snapshot["events"].as_array().unwrap();
            assert_eq!(events.len(), 2);
            assert_step(&events[0], "operational");
            assert_eq!(events[1]["agent_stdout"], true);
            assert_step(&events[1], "initial-agent");
            append(&agent, &relay("snapshot-gap"));
            let mut live = stream(server.get(&format!(
                "/api/log/stream?from={}&agent_from={}",
                snapshot["offset"], snapshot["agent_offset"]
            )));
            let (id, value) = feed_event(&mut live);
            assert_step(&value, "snapshot-gap");
            assert!(id.contains(':'));
            drop(live);
            append(&path, &line("operational-gap"));
            append(&agent, &relay("agent-gap"));
            let mut resumed = stream(
                server
                    .request("GET", "/api/log/stream?from=0&agent_from=0")
                    .header("last-event-id", id)
                    .send()
                    .unwrap(),
            );
            let (_, value) = feed_event(&mut resumed);
            assert_step(&value, "operational-gap");
            let (_, value) = feed_event(&mut resumed);
            assert_step(&value, "agent-gap");
            // Replacement is larger than the previous file; inode detection must
            // reset only this feed rather than relying on a size shrink.
            fs::rename(&agent, fixture.path("orbit-agent.jsonl.old")).unwrap();
            fs::write(
                &agent,
                relay(&format!("rotated-agent-{}", "x".repeat(4096))),
            )
            .unwrap();
            let (_, value) = feed_event(&mut resumed);
            assert_step(&value, "rotated-agent-");
            append(&path, &line("operational-sentinel"));
            let (_, value) = feed_event(&mut resumed);
            assert_step(&value, "operational-sentinel");
        },
    );
}

#[test]
fn split_log_reconnect_preserves_partial_records_in_either_feed() {
    isolated(
        "log::split_log_reconnect_preserves_partial_records_in_either_feed",
        || {
            for partial_agent in [true, false] {
                let fixture = Fixture::new();
                let operational = fixture.path("orbit.jsonl");
                let agent = fixture.path("orbit-agent.jsonl");
                let record = |is_agent: bool, text: &str| {
                    if is_agent {
                        format!(
                            "{}\n",
                            json!({
                                "timestamp":"2026-10-07T01:00:00Z", "level":"INFO", "target":"orbit_engine::activity_job::cli_runner::supervisor",
                                "fields":{"provider":"codex", "stream":"stdout", "line":text},
                            })
                        )
                    } else {
                        line(text)
                    }
                };
                fs::write(&operational, record(false, "initial-operation")).unwrap();
                fs::write(&agent, record(true, "initial-agent")).unwrap();
                let server = fixture.split_log_server();
                let snapshot = json_ok(server.get("/api/log"));
                let mut live = stream(server.get(&format!(
                    "/api/log/stream?from={}&agent_from={}",
                    snapshot["offset"], snapshot["agent_offset"]
                )));
                let (partial, trigger) = if partial_agent {
                    (&agent, &operational)
                } else {
                    (&operational, &agent)
                };
                let pending = record(partial_agent, "completed-after-reconnect");
                let cut = pending.len() - 4;
                append(partial, &pending[..cut]);
                append(trigger, &record(!partial_agent, "first-handshake"));
                let (_, value) = feed_event(&mut live);
                assert_step(&value, "first-handshake");
                // The second handshake crosses a complete polling cycle, so
                // the companion's partial record has been scanned before its
                // raw offset can enter an event ID.
                append(trigger, &record(!partial_agent, "second-handshake"));
                let (id, value) = feed_event(&mut live);
                assert_step(&value, "second-handshake");
                drop(live);
                append(partial, &pending[cut..]);
                let mut resumed = stream(
                    server
                        .request("GET", "/api/log/stream")
                        .header("last-event-id", id)
                        .send()
                        .unwrap(),
                );
                let (_, value) = feed_event(&mut resumed);
                assert_step(&value, "completed-after-reconnect");
                append(trigger, &record(!partial_agent, "sentinel"));
                let (_, value) = feed_event(&mut resumed);
                assert_step(&value, "sentinel");
            }
        },
    );
}

#[test]
fn diagnostics_errors_join_steps_deduplicate_and_keep_windows_separate() {
    isolated(
        "log::diagnostics_errors_join_steps_deduplicate_and_keep_windows_separate",
        || {
            use chrono::{DateTime, Duration, Utc};
            use orbit_common::storage::blob_store::BlobStore;
            use orbit_core::V2AuditEventInsertParams;
            let fixture = Fixture::new();
            let now = Utc::now();
            let recent = now - Duration::minutes(2);
            let old = now - Duration::days(2);
            let insert = |id: &str, run: &str, ts: DateTime<Utc>, mut body: Value| {
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
                        parent_event_id: body["parent_event_id"].as_str().map(str::to_string),
                        workspace_path: None,
                        payload_json: body.to_string(),
                    })
                    .unwrap();
            };
            for (run, message) in [
                ("run-a", "build failed: missing dependency"),
                ("run-b", "validation failed"),
            ] {
                insert(
                    &format!("{run}-start"),
                    run,
                    if run == "run-a" { old } else { recent },
                    json!({"body_kind":"step_started", "step_id":"fulfil"}),
                );
                insert(
                    &format!("{run}-finish"),
                    run,
                    recent + Duration::milliseconds(1),
                    json!({"body_kind":"step_finished", "step_id":"fulfil", "outcome":"error", "error_message":message}),
                );
            }
            let blobs = BlobStore::new(fixture.runtime.data_root().join("state/audit/blobs"));
            for (id, ts) in [("agent-recent", recent), ("agent-old", old)] {
                let ts_text = ts.to_rfc3339();
                let stderr = format!(
                    "{ts_text} ERROR codex_models_manager::manager: failed to refresh available models: request timed out\n{ts_text} ERROR codex_models_manager::manager: failed to refresh available models: request timed out\n{ts_text} ERROR codex_core::tools::router: error=apply_patch verification failed: Failed to find expected lines in src/file.rs\n"
                );
                let blob = blobs.write(stderr.as_bytes()).unwrap();
                insert(
                    id,
                    "run-a",
                    ts,
                    json!({"body_kind":"cli_invocation_finished", "parent_event_id":"run-a-start", "stderr_blob_ref":blob}),
                );
            }
            let process = |run: &str, ts: chrono::DateTime<Utc>, id: &str| {
                json!({
                    "timestamp":ts.to_rfc3339(), "level":"ERROR", "target":"orbit.job.step_finished",
                    "fields":{"job_run_id":run, "step_id":"fulfil", "outcome":"error", "success":false, "event_id":id},
                })
            };
            // Appended in time order, as the log is: the old record comes first.
            let mut records = vec![
                process("run-a", old, "process-old"),
                process("run-a", recent, "process-a"),
                process("run-a", recent, "process-a"),
                process("run-b", recent, "process-b"),
                process("run-future", now + Duration::minutes(10), "future"),
            ];
            records.push(
                json!({"timestamp":recent.to_rfc3339(), "level":"ERROR", "target":"backend",
            "fields":{"error_message":"direct failure", "event_id":"direct"}}),
            );
            fs::write(
                fixture.path("process.log"),
                records
                    .iter()
                    .map(|row| format!("{row}\n"))
                    .collect::<String>(),
            )
            .unwrap();
            let server = fixture.server(false);
            let read = |window: &str, limit| {
                json_ok(server.get(&format!(
                    "/api/diagnostics/errors?since={window}&limit={limit}&workspace=ws_http_fixture"
                )))["items"]
                    .clone()
            };
            let day = read("24h", 50);
            let rows = day.as_array().unwrap();
            assert_eq!(
                rows.len(),
                4,
                "one row per event; older/future errors excluded: {day}"
            );
            let row = |id: &str| rows.iter().find(|row| row["event_id"] == id).unwrap();
            assert_eq!(
                row("process-a")["message"],
                "build failed: missing dependency"
            );
            assert_eq!(row("process-b")["message"], "validation failed");
            assert_eq!(row("process-a")["source"], "process");
            assert_eq!(row("process-a")["target"], "orbit.job.step_finished");
            assert_eq!(row("process-a")["step_index"], 0);
            assert_eq!(row("direct")["message"], "direct failure");
            assert_eq!(
                row("agent-recent")["message"],
                "failed to refresh available models: request timed out\nerror=apply_patch verification failed: Failed to find expected lines in src/file.rs"
            );
            assert_eq!(
                row("agent-recent")["target"],
                "codex_models_manager::manager, codex_core::tools::router"
            );
            assert_eq!(row("agent-recent")["step_index"], 0);
            assert_eq!(
                row("agent-recent")["step"],
                "fulfil",
                "step ancestor before the selected window still attributes the row"
            );
            assert_eq!(read("7d", 50).as_array().unwrap().len(), 6);
            assert_eq!(
                read("24h", 50),
                day,
                "window-specific memo survives neighboring polls"
            );
            assert_eq!(
                read("24h", 1).as_array().unwrap().len(),
                1,
                "limit-specific memo"
            );
            assert!(read("all", 0).as_array().unwrap().is_empty());
            assert_eq!(
                server
                    .get("/api/diagnostics/errors?since=bogus")
                    .status()
                    .as_u16(),
                400
            );
        },
    );
}

#[test]
fn diagnostics_errors_scope_process_rows_to_the_selected_workspaces_runs() {
    isolated(
        "log::diagnostics_errors_scope_process_rows_to_the_selected_workspaces_runs",
        || {
            use chrono::{Duration, Utc};
            use orbit_core::{OrbitRuntime, V2AuditEventInsertParams};
            use orbit_types::workspace::WorkspaceCheckout;
            const OTHER: &str = "ws_http_other";

            let fixture = Fixture::new();
            let registry_path =
                orbit_registry::workspace_registry::registry_path_for(&fixture.global);
            let mut registry =
                orbit_registry::workspace_registry::load_registry_from(&registry_path).unwrap();
            let mut workspace = registry.workspaces[0].clone();
            workspace.id = OTHER.into();
            workspace.name = "other".into();
            let repo = fixture.path("other");
            let work = repo.join(".orbit");
            fs::create_dir_all(&work).unwrap();
            fs::write(
                work.join("config.yaml"),
                format!("schema_version: 1\nworkspace_id: {OTHER}\n"),
            )
            .unwrap();
            let checkout = WorkspaceCheckout::owner(OTHER.into(), repo, work);
            registry.workspaces.push(workspace.clone());
            registry.checkouts.push(checkout.clone());
            orbit_registry::workspace_registry::save_registry_to(&registry, &registry_path)
                .unwrap();
            let other =
                orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout(
                    &fixture.global,
                    &workspace,
                    &checkout,
                )
                .unwrap();

            let recent = Utc::now() - Duration::minutes(2);
            let finish = |runtime: &OrbitRuntime, run: &str, message: &str| {
                for (suffix, body) in [
                    (
                        "start",
                        json!({"body_kind":"step_started", "step_id":"fulfil"}),
                    ),
                    (
                        "finish",
                        json!({"body_kind":"step_finished", "step_id":"fulfil", "outcome":"error", "error_message":message}),
                    ),
                ] {
                    let id = format!("{run}-{suffix}");
                    let mut body = body;
                    body["event_id"] = json!(id);
                    body["ts"] = json!(recent.to_rfc3339());
                    body["run_id"] = json!(run);
                    runtime
                        .insert_v2_audit_event(&V2AuditEventInsertParams {
                            workspace_id: runtime.workspace_id().unwrap(),
                            event_id: id,
                            source: "v2_envelope".into(),
                            schema_version: 1,
                            event_type: body["body_kind"].as_str().unwrap().into(),
                            ts: recent,
                            run_id: run.into(),
                            agent_identity: "http-fixture".into(),
                            parent_event_id: None,
                            workspace_path: None,
                            payload_json: body.to_string(),
                        })
                        .unwrap();
                }
            };
            finish(&fixture.runtime, "run-local", "local step failed");
            finish(&other, "run-foreign", "foreign step failed");

            let process = |run: &str, id: &str| {
                json!({
                    "timestamp":recent.to_rfc3339(), "level":"ERROR", "target":"orbit.job.step_finished",
                    "fields":{"job_run_id":run, "step_id":"fulfil", "outcome":"error", "success":false, "event_id":id},
                })
            };
            let unknown = process("run-unknown", "process-unknown");
            let direct = json!({"timestamp":recent.to_rfc3339(), "level":"ERROR", "target":"backend",
                "fields":{"error_message":"direct failure", "event_id":"direct"}});
            fs::write(
                fixture.path("process.log"),
                [
                    process("run-local", "process-local"),
                    process("run-foreign", "process-foreign"),
                    unknown,
                    direct,
                ]
                .iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
            )
            .unwrap();
            let server = fixture.server(false);
            let rows_for = |workspace: &str| {
                json_ok(server.get(&format!(
                    "/api/diagnostics/errors?since=24h&limit=50&workspace={workspace}"
                )))["items"]
                    .as_array()
                    .unwrap()
                    .clone()
            };
            let describe = |rows: &[Value]| {
                let mut seen: Vec<_> = rows
                    .iter()
                    .map(|row| {
                        (
                            row["event_id"].as_str().unwrap().to_string(),
                            row["message"].as_str().unwrap().to_string(),
                        )
                    })
                    .collect();
                seen.sort();
                seen
            };
            let pair = |id: &str, message: &str| (id.to_string(), message.to_string());
            assert_eq!(
                describe(&rows_for("ws_http_fixture")),
                vec![
                    pair("direct", "direct failure"),
                    pair("process-local", "local step failed"),
                ],
                "a run owned by another workspace is neither listed nor left with the generic step text"
            );
            assert_eq!(
                describe(&rows_for(OTHER)),
                vec![
                    pair("direct", "direct failure"),
                    pair("process-foreign", "foreign step failed"),
                ],
                "the owning workspace shows the same run with its step error"
            );
        },
    );
}

#[test]
fn diagnostics_errors_read_rotated_segments_within_the_window_and_report_retention() {
    isolated(
        "log::diagnostics_errors_read_rotated_segments_within_the_window_and_report_retention",
        || {
            use chrono::{DateTime, Duration, Utc};
            let fixture = Fixture::new();
            let now = Utc::now();
            let record = |ts: DateTime<Utc>, level: &str, id: &str| {
                format!(
                    "{}\n",
                    json!({"timestamp":ts.to_rfc3339(), "level":level, "target":"backend",
                        "fields":{"error_message":format!("{id} failed"), "event_id":id}})
                )
            };
            let segment = |name: &str, records: &[String]| {
                fs::write(fixture.path(name), records.concat()).unwrap();
            };
            let archive = |rotated_at: DateTime<Utc>| {
                format!("process.log.{}", rotated_at.format("%Y%m%dT%H%M%S%3fZ"))
            };
            let oldest_start = now - Duration::hours(30);
            segment(
                "process.log",
                &[
                    record(now - Duration::hours(1), "INFO", "active-start"),
                    record(now - Duration::minutes(5), "ERROR", "active"),
                ],
            );
            segment(
                &archive(now - Duration::hours(1)),
                &[
                    record(now - Duration::hours(6), "INFO", "rotated-start"),
                    record(now - Duration::hours(5), "ERROR", "rotated-in-day"),
                ],
            );
            segment(
                &archive(now - Duration::hours(6)),
                &[
                    record(oldest_start, "INFO", "oldest-start"),
                    record(now - Duration::hours(29), "ERROR", "rotated-in-week"),
                ],
            );
            let server = fixture.server(false);
            let read = |window: &str| {
                json_ok(server.get(&format!(
                    "/api/diagnostics/errors?since={window}&limit=50&workspace=ws_http_fixture"
                )))
            };
            let ids = |body: &Value| {
                body["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row["event_id"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            };
            let instant = |value: &Value| {
                value
                    .as_str()
                    .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                    .map(|ts| ts.with_timezone(&Utc))
            };

            let day = read("24h");
            assert_eq!(
                ids(&day),
                ["active", "rotated-in-day"],
                "the 24h window includes the rotated segment it reaches into: {day}"
            );
            assert!(day["since"].is_string(), "{day}");
            assert_eq!(
                day["coverage_since"], day["since"],
                "a retained segment starts before the window, so coverage is complete: {day}"
            );

            let week = read("7d");
            assert_eq!(
                ids(&week),
                ["active", "rotated-in-day", "rotated-in-week"],
                "{week}"
            );
            assert_eq!(
                instant(&week["coverage_since"]),
                Some(oldest_start),
                "retention starts after the 7d window start, so coverage begins at the oldest retained record: {week}"
            );
            assert!(instant(&week["since"]).unwrap() < oldest_start, "{week}");
        },
    );
}

#[test]
fn diagnostics_errors_report_the_stderr_blob_cap_as_the_coverage_start() {
    isolated(
        "log::diagnostics_errors_report_the_stderr_blob_cap_as_the_coverage_start",
        || {
            use chrono::{DateTime, Duration, Utc};
            use orbit_common::storage::blob_store::BlobStore;
            use orbit_core::V2AuditEventInsertParams;
            // One past the per-request stderr blob cap in the web crate.
            const INVOCATIONS: i64 = 257;
            let fixture = Fixture::new();
            let now = Utc::now();
            let blobs = BlobStore::new(fixture.runtime.data_root().join("state/audit/blobs"));
            let invoked_at = |index: i64| now - Duration::minutes(index + 1);
            for index in 0..INVOCATIONS {
                let ts = invoked_at(index);
                let id = format!("invocation-{index}");
                let blob = blobs
                    .write(format!("plain stderr from invocation {index}\n").as_bytes())
                    .unwrap();
                let body = json!({"body_kind":"cli_invocation_finished", "event_id":id,
                    "ts":ts.to_rfc3339(), "run_id":"run-a", "stderr_blob_ref":blob});
                fixture
                    .runtime
                    .insert_v2_audit_event(&V2AuditEventInsertParams {
                        workspace_id: fixture.runtime.workspace_id().unwrap(),
                        event_id: id,
                        source: "v2_envelope".into(),
                        schema_version: 1,
                        event_type: "cli_invocation_finished".into(),
                        ts,
                        run_id: "run-a".into(),
                        agent_identity: "http-fixture".into(),
                        parent_event_id: None,
                        workspace_path: None,
                        payload_json: body.to_string(),
                    })
                    .unwrap();
            }
            let server = fixture.server(false);
            let body = json_ok(
                server.get("/api/diagnostics/errors?since=24h&limit=50&workspace=ws_http_fixture"),
            );
            let instant = |value: &Value| {
                value
                    .as_str()
                    .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
                    .map(|ts| ts.with_timezone(&Utc))
            };
            assert_eq!(
                instant(&body["coverage_since"]),
                Some(invoked_at(INVOCATIONS - 2)),
                "the oldest invocation whose stderr was read starts the coverage: {body}"
            );
            assert!(
                instant(&body["since"]).unwrap() < invoked_at(INVOCATIONS - 1),
                "{body}"
            );
        },
    );
}
