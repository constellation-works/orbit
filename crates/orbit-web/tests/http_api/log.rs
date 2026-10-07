use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::Command;

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
            id = Some(value.trim().parse().unwrap());
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
            let errors = json_ok(response);
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
