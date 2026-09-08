use std::io::Write;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::tempdir;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

use super::super::LogQuery;
use super::super::log::{
    LOG_MAX_LIMIT, LogStreamGate, format_sse_frame, last_event_id_header, log_stream_unavailable,
    read_appended_log_events, read_log_snapshot_from_path, spawn_log_sse_frames,
    stream_resume_offset,
};
use super::test_support::{body_json, write_lines};
use crate::log_format::Filters as LogFilters;

fn log_line(step_id: &str) -> String {
    json!({
        "timestamp": "2026-04-27T01:00:05Z",
        "level": "INFO",
        "target": "orbit.job.step_started",
        "fields": {"job_run_id": "run-1", "step_id": step_id}
    })
    .to_string()
}

fn collect_sse_frames(rx: &mut tokio::sync::mpsc::Receiver<String>, n: usize) -> Vec<String> {
    let mut frames = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while frames.len() < n && Instant::now() < deadline {
        match rx.try_recv() {
            Ok(frame) => frames.push(frame),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
    frames
}

#[test]
fn log_snapshot_filters_target_level_and_since() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(
        &path,
        &[
            json!({
                "timestamp": "2026-04-27T01:00:01Z",
                "level": "INFO",
                "target": "orbit.policy.deny",
                "fields": {"tool": "proc.spawn", "path": "/tmp/a"}
            })
            .to_string(),
            json!({
                "timestamp": "2026-04-27T01:00:03Z",
                "level": "WARN",
                "target": "orbit.policy.deny",
                "fields": {"tool": "fs.write", "path": "/etc/passwd"}
            })
            .to_string(),
            json!({
                "timestamp": "2026-04-27T01:00:04Z",
                "level": "ERROR",
                "target": "orbit.job.step_finished",
                "fields": {"step_id": "build", "outcome": "failed", "success": false}
            })
            .to_string(),
        ],
    );

    let snapshot = read_log_snapshot_from_path(
        &path,
        &LogQuery {
            limit: Some(10),
            target: Some("orbit.policy".to_string()),
            level: Some("warn".to_string()),
            since: Some("2026-04-27T01:00:02Z".to_string()),
            from: None,
        },
    )
    .expect("snapshot");

    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(snapshot.events[0].source, "policy");
    assert_eq!(snapshot.events[0].code, "DENY");
    assert_eq!(snapshot.events[0].level, "warn");
    assert!(snapshot.events[0].message_html.contains("<b>path</b>="));
    assert_eq!(
        snapshot.offset,
        std::fs::metadata(&path).expect("metadata").len()
    );
}

#[test]
fn log_snapshot_rejects_limit_above_max() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[]);

    let err = read_log_snapshot_from_path(
        &path,
        &LogQuery {
            limit: Some(LOG_MAX_LIMIT + 1),
            ..LogQuery::default()
        },
    )
    .expect_err("limit should be rejected");

    assert!(err.to_string().contains("limit must be <= 500"));
}

#[test]
fn log_stream_framing_emits_one_data_frame_per_appended_line() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[]);
    let mut offset = std::fs::metadata(&path).expect("metadata").len();
    let mut leftover = String::new();

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    writeln!(
        file,
        "{}",
        json!({
            "timestamp": "2026-04-27T01:00:05Z",
            "level": "INFO",
            "target": "orbit.job.step_started",
            "fields": {"job_run_id": "run-1", "step_id": "build"}
        })
    )
    .expect("write event");
    file.flush().expect("flush");

    let events =
        read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut leftover)
            .expect("read appended");
    assert_eq!(events.len(), 1);

    let (event, event_offset) = &events[0];
    let frame = format_sse_frame(event, *event_offset).expect("frame");
    assert!(
        frame.starts_with(&format!("id: {event_offset}\n")),
        "SSE frame must carry the byte offset as last-event-id: {frame}"
    );
    assert!(frame.contains("\ndata: "));
    assert!(frame.ends_with("\n\n"));
    assert!(frame.contains("\"source\":\"job\""));
    assert!(frame.contains("build"));
}

#[test]
fn log_stream_gate_caps_concurrent_acquisitions() {
    let gate = LogStreamGate::new(2);
    assert_eq!(gate.available_permits(), 2);

    let p1 = gate.try_acquire().expect("first permit available");
    let p2 = gate.try_acquire().expect("second permit available");
    assert_eq!(gate.available_permits(), 0);

    assert!(
        gate.try_acquire().is_none(),
        "third acquisition must be rejected when the cap is reached"
    );

    drop(p1);
    assert_eq!(gate.available_permits(), 1);
    let p3 = gate
        .try_acquire()
        .expect("permit available after one was released");

    drop(p2);
    drop(p3);
    assert_eq!(
        gate.available_permits(),
        2,
        "all permits return to the gate once stream owners drop them"
    );
}

#[tokio::test]
async fn log_stream_unavailable_returns_503_with_retry_after() {
    let response = log_stream_unavailable();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let retry = response
        .headers()
        .get(header::RETRY_AFTER)
        .expect("Retry-After header set");
    assert_eq!(retry.to_str().expect("ascii Retry-After"), "5");
    let body = body_json(response).await;
    assert!(
        body.get("error")
            .and_then(|v| v.as_str())
            .is_some_and(|s| s.contains("concurrency limit reached")),
        "error message names the concurrency limit: {body}"
    );
}

#[test]
fn last_event_id_wins_over_from_query() {
    assert_eq!(stream_resume_offset(Some(10), Some("25")), Some(25));
    assert_eq!(stream_resume_offset(Some(10), Some("nope")), Some(10));
    assert_eq!(stream_resume_offset(Some(10), Some(" 8 ")), Some(8));
    assert_eq!(stream_resume_offset(Some(10), None), Some(10));
    assert_eq!(stream_resume_offset(None, None), None);
    assert_eq!(stream_resume_offset(None, Some("")), None);
}

#[test]
fn last_event_id_header_reads_sse_reconnect_header() {
    let mut headers = HeaderMap::new();
    headers.insert("Last-Event-ID", HeaderValue::from_static("42"));
    assert_eq!(last_event_id_header(&headers), Some("42"));
}

#[test]
fn lines_written_between_snapshot_and_stream_open_all_arrive() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[log_line("seed")]);

    let snapshot = read_log_snapshot_from_path(
        &path,
        &LogQuery {
            limit: Some(50),
            ..LogQuery::default()
        },
    )
    .expect("snapshot");
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(
        snapshot.offset,
        std::fs::metadata(&path).expect("metadata").len()
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("append");
    const GAP_LINES: usize = 3;
    for i in 1..=GAP_LINES {
        writeln!(file, "{}", log_line(&format!("gap-{i}"))).expect("write gap line");
    }
    file.flush().expect("flush");

    let gate = LogStreamGate::new(1);
    let permit = gate.try_acquire().expect("stream permit");
    let mut rx = spawn_log_sse_frames(
        path.clone(),
        LogFilters::default(),
        permit,
        Some(snapshot.offset),
    );
    let frames = collect_sse_frames(&mut rx, GAP_LINES);
    drop(rx);

    assert_eq!(
        frames.len(),
        GAP_LINES,
        "every line written between snapshot and stream open must arrive: {frames:?}"
    );
    for i in 1..=GAP_LINES {
        assert!(
            frames[i - 1].contains(&format!("gap-{i}")),
            "frame {i} missing gap-{i}: {}",
            frames[i - 1]
        );
        assert!(
            frames[i - 1].starts_with("id: "),
            "frame {i} must include an SSE id: {}",
            frames[i - 1]
        );
    }
    assert!(
        frames.iter().all(|frame| !frame.contains("seed")),
        "snapshot-already-read seed line must not be replayed: {frames:?}"
    );
}

#[test]
fn reconnect_with_last_event_id_replays_only_lines_after_offset() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(
        &path,
        &[log_line("keep-0"), log_line("after-1"), log_line("after-2")],
    );

    let mut offset = 0;
    let mut leftover = String::new();
    let events =
        read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut leftover)
            .expect("read all");
    assert_eq!(events.len(), 3);
    let last_event_id = events[0].1.to_string();

    let mut headers = HeaderMap::new();
    headers.insert(
        "Last-Event-ID",
        HeaderValue::from_str(&last_event_id).expect("header"),
    );
    let resume = stream_resume_offset(Some(0), last_event_id_header(&headers));
    assert_eq!(resume, Some(events[0].1));

    let gate = LogStreamGate::new(1);
    let permit = gate.try_acquire().expect("stream permit");
    let mut rx = spawn_log_sse_frames(path, LogFilters::default(), permit, resume);
    let frames = collect_sse_frames(&mut rx, 2);
    drop(rx);

    assert_eq!(
        frames.len(),
        2,
        "expected the two lines after the id: {frames:?}"
    );
    assert!(
        frames[0].contains("after-1"),
        "first replayed frame should be after-1: {}",
        frames[0]
    );
    assert!(
        frames[1].contains("after-2"),
        "second replayed frame should be after-2: {}",
        frames[1]
    );
    assert!(
        frames.iter().all(|frame| !frame.contains("keep-0")),
        "Last-Event-ID must not replay the line at that offset: {frames:?}"
    );
}

#[test]
fn log_snapshot_skips_malformed_records_and_preserves_order() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(
        &path,
        &[
            log_line("keep-1"),
            "not-json".to_string(),
            "{".to_string(),
            log_line("keep-2"),
        ],
    );

    let snapshot = read_log_snapshot_from_path(
        &path,
        &LogQuery {
            limit: Some(10),
            ..LogQuery::default()
        },
    )
    .expect("snapshot");

    assert_eq!(snapshot.events.len(), 2);
    assert!(snapshot.events[0].message_html.contains("keep-1"));
    assert!(snapshot.events[1].message_html.contains("keep-2"));
    assert_eq!(
        snapshot.offset,
        std::fs::metadata(&path).expect("metadata").len()
    );
}

#[test]
fn log_snapshot_includes_final_line_without_trailing_newline() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    let body = format!("{}\n{}", log_line("first"), log_line("last"));
    std::fs::write(&path, body).expect("write");

    let snapshot = read_log_snapshot_from_path(
        &path,
        &LogQuery {
            limit: Some(10),
            ..LogQuery::default()
        },
    )
    .expect("snapshot");

    assert_eq!(snapshot.events.len(), 2);
    assert!(snapshot.events[0].message_html.contains("first"));
    assert!(snapshot.events[1].message_html.contains("last"));
}

#[test]
fn log_snapshot_zero_limit_returns_no_events() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[log_line("seed")]);

    let snapshot = read_log_snapshot_from_path(
        &path,
        &LogQuery {
            limit: Some(0),
            ..LogQuery::default()
        },
    )
    .expect("snapshot");

    assert!(snapshot.events.is_empty());
    assert_eq!(
        snapshot.offset,
        std::fs::metadata(&path).expect("metadata").len()
    );
}

#[test]
fn log_snapshot_missing_file_returns_empty_events() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("missing.jsonl");

    let snapshot = read_log_snapshot_from_path(
        &path,
        &LogQuery {
            limit: Some(10),
            ..LogQuery::default()
        },
    )
    .expect("snapshot");

    assert!(snapshot.events.is_empty());
    assert_eq!(snapshot.offset, 0);
}

#[tokio::test]
async fn log_snapshot_scan_runs_through_blocking_boundary() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[log_line("blocked")]);
    let query = LogQuery {
        limit: Some(10),
        ..LogQuery::default()
    };

    let snapshot = super::super::blocking("log snapshot", {
        let path = path.clone();
        move || read_log_snapshot_from_path(&path, &query)
    })
    .await
    .unwrap_or_else(|_| panic!("blocking snapshot"));

    assert_eq!(snapshot.events.len(), 1);
    assert!(snapshot.events[0].message_html.contains("blocked"));
}
