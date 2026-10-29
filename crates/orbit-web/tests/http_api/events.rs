//! `GET /api/events/stream`: workspace change notices over SSE. The server is
//! a separate process, so every write below, made through this test's own
//! runtime as the CLI makes it, is another process's write to the server.

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{JobRun, JobRunState, TaskStatus};
use reqwest::blocking::Response;
use serde_json::{Value, json};

use super::support::{Fixture, Server, isolated};

const WS: &str = "?workspace=ws_http_fixture";
/// Bounds every stream read: a notice is due within one scan interval.
const STREAM_TIMEOUT: Duration = Duration::from_secs(60);

fn open(server: &Server, last_event_id: Option<&str>) -> BufReader<Response> {
    let mut request = server
        .request("GET", &format!("/api/events/stream{WS}"))
        .header("accept-encoding", "gzip")
        .timeout(STREAM_TIMEOUT);
    if let Some(id) = last_event_id {
        request = request.header("last-event-id", id);
    }
    let response = request.send().unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    assert!(
        response.headers().get("content-encoding").is_none(),
        "SSE is never compressed, so each notice arrives as it is sent"
    );
    BufReader::new(response)
}

/// The next notice and its event id, skipping `retry:` and comment frames.
fn next_notice(reader: &mut BufReader<Response>) -> (String, Value) {
    let mut id: Option<String> = None;
    let mut data: Option<String> = None;
    loop {
        let mut line = String::new();
        assert!(
            reader
                .read_line(&mut line)
                .expect("SSE line within timeout")
                > 0,
            "SSE stream ended before a notice"
        );
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            if let Some(data) = data.take() {
                let id = id.take().expect("every notice carries an event id");
                return (id, serde_json::from_str(&data).expect("notice is JSON"));
            }
            continue;
        }
        if let Some(value) = line.strip_prefix("id: ") {
            id = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("data: ") {
            data = Some(value.to_owned());
        }
    }
}

/// Read notices until one matches; returns it with its id and every notice read.
fn await_notice(
    reader: &mut BufReader<Response>,
    what: &str,
    matches: impl Fn(&Value) -> bool,
) -> (String, Value, Vec<Value>) {
    let mut seen = Vec::new();
    loop {
        let (id, notice) = next_notice(reader);
        seen.push(notice.clone());
        if matches(&notice) {
            return (id, notice, seen);
        }
        assert!(seen.len() < 200, "no {what} notice among {seen:?}");
    }
}

fn kind(notice: &Value) -> &str {
    notice["kind"].as_str().unwrap_or_default()
}

fn set_status(fixture: &Fixture, id: &str, status: TaskStatus) {
    fixture
        .runtime
        .update_task_as_human(
            id,
            TaskUpdateParams {
                status: Some(status),
                ..TaskUpdateParams::default()
            },
            "test operator".into(),
        )
        .unwrap();
}

fn drain_run(fixture: &Fixture, state: JobRunState, task: &str) -> JobRun {
    let mut run = fixture.seed_run("jrun-events-drain", "workspace_auto_pipeline", state);
    run.input = Some(json!({"task_id": task}));
    fixture.save_run(&run);
    run
}

#[test]
fn change_stream_notices_other_process_writes_and_resumes_after_reconnect() {
    isolated(
        "events::change_stream_notices_other_process_writes_and_resumes_after_reconnect",
        || {
            let fixture = Fixture::new();
            let task = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Watched task".into(),
                    description: "Changes from outside the server.".into(),
                    status: Some(TaskStatus::Backlog),
                    ..TaskAddParams::default()
                })
                .unwrap();
            let server = fixture.server(false);
            let mut stream = open(&server, None);

            // Connect: the drain phase, then the host sample, both level state.
            let (_, drain, _) = await_notice(&mut stream, "drain", |n| kind(n) == "drain");
            assert_eq!(drain["phase"], "idle", "{drain}");
            let (_, host, _) = await_notice(&mut stream, "host.resources", |n| {
                kind(n) == "host.resources"
            });
            assert!(host["resources"]["thresholds"].is_object(), "{host}");

            // A task status change from this process: its task notice, and
            // locks, since an in-progress task holds its files.
            set_status(&fixture, &task.id, TaskStatus::InProgress);
            let (_, changed, seen) = await_notice(&mut stream, "task", |n| {
                kind(n) == "task" && n["id"] == task.id.as_str()
            });
            assert_eq!(changed["status"], "in-progress", "{changed}");
            assert!(changed["updated_at"].is_string(), "{changed}");
            if !seen.iter().any(|n| kind(n) == "locks") {
                await_notice(&mut stream, "locks", |n| kind(n) == "locks");
            }

            // A drain starting is a run notice and a drain phase change.
            let run = drain_run(&fixture, JobRunState::Running, &task.id);
            let (_, started, seen) = await_notice(&mut stream, "run", |n| {
                kind(n) == "run" && n["id"] == run.run_id.as_str()
            });
            assert_eq!(started["state"], "running", "{started}");
            assert_eq!(started["task_id"], task.id.as_str(), "{started}");
            let draining = |n: &Value| kind(n) == "drain" && n["phase"] == "draining";
            if !seen.iter().any(draining) {
                await_notice(&mut stream, "draining", draining);
            }
            drain_run(&fixture, JobRunState::Success, &task.id);
            let (_, finished, seen) = await_notice(&mut stream, "run success", |n| {
                kind(n) == "run" && n["state"] == "success"
            });
            assert_eq!(finished["id"], run.run_id.as_str());
            let idle = |n: &Value| kind(n) == "drain" && n["phase"] == "idle";
            if !seen.iter().any(idle) {
                await_notice(&mut stream, "idle", idle);
            }

            // Disconnected, nothing reaches the client. Resuming from the last
            // id it saw replays what it missed.
            let (last_id, _, _) = await_notice(&mut stream, "any", |_| true);
            drop(stream);
            set_status(&fixture, &task.id, TaskStatus::Review);
            let mut resumed = open(&server, Some(&last_id));
            let (_, missed, seen) = await_notice(&mut resumed, "missed task", |n| {
                kind(n) == "task" && n["id"] == task.id.as_str()
            });
            assert_eq!(
                missed["status"], "review",
                "the replay is the current state"
            );
            assert!(
                !seen.iter().any(|n| kind(n) == "resync"),
                "a recent id resumes without a resync: {seen:?}"
            );
            drop(resumed);

            // An id the server cannot place gets a resync instead.
            let future = (chrono::Utc::now() + chrono::Duration::days(1))
                .timestamp_millis()
                .to_string();
            for id in ["not-an-id", future.as_str()] {
                let mut unknown = open(&server, Some(id));
                let (_, resync, _) = await_notice(&mut unknown, "resync", |n| kind(n) == "resync");
                assert_eq!(resync["reason"], "unknown_event_id", "{id}: {resync}");
            }
        },
    );
}

#[test]
fn change_streams_are_capped_and_close_on_shutdown() {
    isolated(
        "events::change_streams_are_capped_and_close_on_shutdown",
        || {
            let fixture = Fixture::new();
            let mut server = fixture.server(false);
            let mut streams = Vec::new();
            let refused = loop {
                let response = server
                    .request("GET", &format!("/api/events/stream{WS}"))
                    .timeout(STREAM_TIMEOUT)
                    .send()
                    .unwrap();
                if response.status() != 200 {
                    break response;
                }
                streams.push(BufReader::new(response));
                assert!(streams.len() <= 64, "the stream count must be capped");
            };
            assert!(!streams.is_empty(), "the cap admits streams");
            assert_eq!(refused.status(), 503);
            assert_eq!(refused.headers()["retry-after"], "5");

            // Each open stream closes itself on SIGTERM, so the server exits well
            // before its bounded graceful-drain deadline would cut them off.
            let started = Instant::now();
            assert!(server.terminate().success());
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "open change streams held shutdown for {:?}",
                started.elapsed()
            );
            for mut stream in streams {
                let mut rest = String::new();
                while stream.read_line(&mut rest).is_ok_and(|read| read > 0) {}
            }
        },
    );
}
