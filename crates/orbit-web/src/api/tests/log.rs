use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::json;
use tempfile::tempdir;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};

use super::super::LogQuery;
use super::super::log::{
    LOG_MAX_LIMIT, LOG_STREAM_BATCH_BYTES, LOG_STREAM_BATCH_EVENTS, LOG_STREAM_MAX_RECORD_BYTES,
    LogLineBuffer, LogSnapshot, LogStreamGate, PollBackoff, format_sse_frame, last_event_id_header,
    log_stream_unavailable, read_appended_log_events, read_log_snapshot_from_path,
    read_log_snapshot_then, spawn_log_sse_frames, stream_resume_offset,
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

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .expect("append");
    file.write_all(text.as_bytes()).expect("append bytes");
    file.flush().expect("flush");
}

fn snapshot(path: &Path, query: LogQuery) -> LogSnapshot {
    read_log_snapshot_from_path(path, &query).expect("snapshot")
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).expect("metadata").len()
}

/// Resume a stream at `offset`, run `after_open`, then append a sentinel and
/// return every frame before it. Collecting up to a known last line means a
/// duplicate or extra frame cannot hide behind a timeout.
fn stream_frames_until_sentinel(
    path: &Path,
    filters: LogFilters,
    offset: u64,
    after_open: impl FnOnce(),
) -> Vec<String> {
    let gate = LogStreamGate::new(1);
    let permit = gate.try_acquire().expect("stream permit");
    let mut rx = spawn_log_sse_frames(path.to_path_buf(), filters, permit, Some(offset));
    after_open();
    append(path, &format!("{}\n", log_line("sentinel")));
    let mut frames = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(frame) if frame.contains("sentinel") => return frames,
            Ok(frame) => frames.push(frame),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
        }
    }
    panic!("sentinel never arrived; frames so far: {frames:?}");
}

/// Occurrences of `step` across the snapshot events and the streamed frames.
fn delivered(snapshot: &LogSnapshot, frames: &[String], step: &str) -> usize {
    snapshot
        .events
        .iter()
        .filter(|event| event.message_html.contains(step))
        .count()
        + frames.iter().filter(|frame| frame.contains(step)).count()
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
    let mut lines = LogLineBuffer::default();

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

    let events = read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut lines)
        .expect("read appended")
        .events;
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
fn log_stream_skips_invalid_utf8_instead_of_stalling() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    let mut bytes = b"torn \xE2\x82 write\n".to_vec();
    bytes.extend_from_slice(log_line("after-torn").as_bytes());
    bytes.push(b'\n');
    // A trailing partial line split inside a multi-byte character.
    bytes.extend_from_slice("{\"partial\":\"\u{20ac}".as_bytes().split_at(13).0);
    std::fs::write(&path, &bytes).expect("write log");

    let mut offset = 0;
    let mut lines = LogLineBuffer::default();
    let events = read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut lines)
        .expect("invalid UTF-8 must not fail the read")
        .events;

    assert_eq!(events.len(), 1, "the valid line after a torn one is served");
    assert_eq!(
        offset,
        bytes.len() as u64,
        "the stream advances past all bytes"
    );
    assert!(
        !lines.partial.is_empty(),
        "the partial line waits for its newline"
    );
}

fn policy_line(n: usize) -> String {
    json!({
        "timestamp": "2026-04-27T01:00:01Z",
        "level": "WARN",
        "target": "orbit.policy.deny",
        "fields": {"tool": "fs.write", "path": format!("/tmp/{n}")}
    })
    .to_string()
}

#[test]
fn appended_read_renders_at_most_one_batch_per_call() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    let total = LOG_STREAM_BATCH_EVENTS * 3 + 5;
    let lines: Vec<String> = (0..total).map(|i| log_line(&format!("step-{i}"))).collect();
    write_lines(&path, &lines);

    let mut offset = 0;
    let mut buffer = LogLineBuffer::default();
    let mut seen = 0;
    let mut calls = 0;
    loop {
        let batch =
            read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut buffer)
                .expect("read batch");
        calls += 1;
        assert!(
            batch.events.len() <= LOG_STREAM_BATCH_EVENTS,
            "one call rendered {} events",
            batch.events.len()
        );
        for (event, _) in &batch.events {
            assert!(
                event.message_html.contains(&format!("step-{seen}<")),
                "events arrive in order: expected step-{seen}, got {}",
                event.message_html
            );
            seen += 1;
        }
        if !batch.more {
            break;
        }
        assert_eq!(
            batch.events.len(),
            LOG_STREAM_BATCH_EVENTS,
            "a batch cut short by the event cap reports more"
        );
    }
    assert_eq!(seen, total);
    assert_eq!(calls, 4, "{total} events take four bounded reads");
    assert_eq!(offset, file_len(&path));
}

#[test]
fn appended_read_yields_after_its_byte_budget_when_nothing_matches() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    let mut lines = Vec::new();
    let mut bytes = 0u64;
    while bytes <= LOG_STREAM_BATCH_BYTES * 2 {
        let line = policy_line(lines.len());
        bytes += line.len() as u64 + 1;
        lines.push(line);
    }
    lines.push(log_line("after-scan"));
    write_lines(&path, &lines);
    let filters =
        LogFilters::from_query_parts(Some("orbit.job".to_string()), None, None).expect("filters");

    let mut offset = 0;
    let mut buffer = LogLineBuffer::default();
    let first =
        read_appended_log_events(&path, &filters, &mut offset, &mut buffer).expect("first batch");
    assert!(first.events.is_empty());
    assert!(first.more, "a scan stopped by its byte budget reports more");
    assert!(
        offset <= LOG_STREAM_BATCH_BYTES,
        "one call scanned {offset} bytes, past the {LOG_STREAM_BATCH_BYTES}-byte budget"
    );

    let mut matched = Vec::new();
    loop {
        let batch =
            read_appended_log_events(&path, &filters, &mut offset, &mut buffer).expect("batch");
        matched.extend(batch.events);
        if !batch.more {
            break;
        }
    }
    assert_eq!(matched.len(), 1, "records split across budgets reassemble");
    assert!(matched[0].0.message_html.contains("after-scan"));
    assert_eq!(matched[0].1, file_len(&path));
}

#[test]
fn over_limit_unterminated_record_is_bounded_then_skipped_through_its_newline() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[log_line("before")]);
    let filler = "a".repeat(LOG_STREAM_MAX_RECORD_BYTES / 2 + 1);
    append(&path, "{\"runaway\":\"");

    let mut offset = 0;
    let mut buffer = LogLineBuffer::default();
    let mut delivered = Vec::new();
    // The record keeps growing without a newline across several polls.
    for _ in 0..4 {
        append(&path, &filler);
        loop {
            let batch =
                read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut buffer)
                    .expect("read");
            delivered.extend(batch.events);
            assert!(
                buffer.partial.capacity() <= LOG_STREAM_MAX_RECORD_BYTES,
                "partial-line storage grew to {} bytes",
                buffer.partial.capacity()
            );
            if !batch.more {
                break;
            }
        }
    }
    assert!(buffer.discarding, "the over-limit record is being skipped");
    assert_eq!(
        offset,
        file_len(&path),
        "skipped bytes still advance the cursor"
    );

    append(&path, "\"}\n");
    append(&path, &format!("{}\n", log_line("after-oversized")));
    let batch = read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut buffer)
        .expect("read after newline");
    delivered.extend(batch.events);

    let steps: Vec<&str> = delivered
        .iter()
        .map(|(event, _)| event.message_html.as_str())
        .collect();
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(steps[0].contains("before"), "{steps:?}");
    assert!(
        steps[1].contains("after-oversized"),
        "the record after the oversized one is served whole: {steps:?}"
    );
    assert!(!buffer.discarding);
    assert!(buffer.partial.is_empty());
    assert_eq!(delivered[1].1, file_len(&path));
}

#[test]
fn stream_replays_a_log_larger_than_one_batch_and_stops_after_disconnect() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    let total = LOG_STREAM_BATCH_EVENTS * 2 + 10;
    let lines: Vec<String> = (0..total)
        .map(|i| log_line(&format!("replay-{i}")))
        .collect();
    write_lines(&path, &lines);

    let gate = LogStreamGate::new(1);
    let permit = gate.try_acquire().expect("stream permit");
    let mut rx = spawn_log_sse_frames(path.clone(), LogFilters::default(), permit, Some(0));
    let frames = collect_sse_frames(&mut rx, total);
    assert_eq!(frames.len(), total, "every replayed record arrives");
    for (i, frame) in frames.iter().enumerate() {
        assert!(
            frame.contains(&format!("replay-{i}<")),
            "frame {i} out of order: {frame}"
        );
    }

    // More history than the channel holds, then the client goes away.
    let more: Vec<String> = (0..total).map(|i| log_line(&format!("late-{i}"))).collect();
    append(&path, &format!("{}\n", more.join("\n")));
    let _ = collect_sse_frames(&mut rx, 1);
    drop(rx);
    let deadline = Instant::now() + Duration::from_secs(2);
    while gate.available_permits() == 0 {
        assert!(
            Instant::now() < deadline,
            "the replay thread kept running after the client disconnected"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
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
    let mut lines = LogLineBuffer::default();
    let events = read_appended_log_events(&path, &LogFilters::default(), &mut offset, &mut lines)
        .expect("read all")
        .events;
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

    let snapshot = snapshot(
        &path,
        LogQuery {
            limit: Some(10),
            ..LogQuery::default()
        },
    );

    assert_eq!(snapshot.events.len(), 2);
    assert!(snapshot.events[0].message_html.contains("first"));
    assert!(snapshot.events[1].message_html.contains("last"));
    assert_eq!(
        snapshot.offset,
        file_len(&path),
        "a complete unterminated record is served, so the cursor moves past it"
    );

    // Its newline arrives later and must not replay the record.
    let frames =
        stream_frames_until_sentinel(&path, LogFilters::default(), snapshot.offset, || {
            append(&path, &format!("\n{}\n", log_line("next")));
        });
    assert_eq!(delivered(&snapshot, &frames, "last"), 1, "{frames:?}");
    assert_eq!(delivered(&snapshot, &frames, "next"), 1, "{frames:?}");
}

#[test]
fn log_snapshot_zero_limit_returns_no_events() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[log_line("seed")]);
    let zero = || LogQuery {
        limit: Some(0),
        ..LogQuery::default()
    };

    let terminated = snapshot(&path, zero());
    assert!(terminated.events.is_empty());
    assert_eq!(terminated.offset, file_len(&path));

    let records_end = file_len(&path);
    let partial = log_line("partial");
    let (head, rest) = partial.split_at(partial.len() / 2);
    append(&path, head);
    let torn = snapshot(&path, zero());
    assert!(torn.events.is_empty());
    assert_eq!(
        torn.offset, records_end,
        "zero limit still leaves a partial record for the stream"
    );
    let frames = stream_frames_until_sentinel(&path, LogFilters::default(), torn.offset, || {
        append(&path, &format!("{rest}\n"));
    });
    assert_eq!(delivered(&torn, &frames, "partial"), 1, "{frames:?}");
    assert_eq!(delivered(&torn, &frames, "seed"), 0, "{frames:?}");
}

#[test]
fn line_appended_after_snapshot_scan_arrives_exactly_once() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[log_line("seed")]);
    let scanned_len = file_len(&path);

    // Append after the scan fixed its extent but before the response is
    // built: the window where a later `stat` used to swallow the line.
    let snapshot = read_log_snapshot_then(
        &path,
        &LogQuery {
            limit: Some(50),
            ..LogQuery::default()
        },
        || append(&path, &format!("{}\n", log_line("raced"))),
    )
    .expect("snapshot");
    assert_eq!(snapshot.offset, scanned_len);

    let frames = stream_frames_until_sentinel(&path, LogFilters::default(), snapshot.offset, || {});
    assert_eq!(delivered(&snapshot, &frames, "seed"), 1, "{frames:?}");
    assert_eq!(
        delivered(&snapshot, &frames, "raced"),
        1,
        "a line appended during snapshot construction must arrive exactly once: {frames:?}"
    );
}

#[test]
fn partial_trailing_record_completes_exactly_once_after_handoff() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(&path, &[log_line("seed")]);
    let records_end = file_len(&path);
    let partial = log_line("completed");
    let (head, rest) = partial.split_at(partial.len() / 2);
    append(&path, head);

    let snapshot = snapshot(
        &path,
        LogQuery {
            limit: Some(50),
            ..LogQuery::default()
        },
    );
    assert_eq!(snapshot.events.len(), 1);
    assert_eq!(
        snapshot.offset, records_end,
        "the cursor stops at the start of the partial record"
    );

    let frames =
        stream_frames_until_sentinel(&path, LogFilters::default(), snapshot.offset, || {
            // Let the stream poll the partial bytes before the record completes.
            std::thread::sleep(Duration::from_millis(150));
            append(&path, &format!("{rest}\n"));
        });
    assert_eq!(delivered(&snapshot, &frames, "seed"), 1, "{frames:?}");
    assert_eq!(delivered(&snapshot, &frames, "completed"), 1, "{frames:?}");
}

#[test]
fn filtered_snapshot_hands_off_trailing_records_exactly_once() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    let policy = json!({
        "timestamp": "2026-04-27T01:00:01Z",
        "level": "WARN",
        "target": "orbit.policy.deny",
        "fields": {"tool": "fs.write", "path": "/etc/passwd"}
    })
    .to_string();
    write_lines(&path, &[log_line("kept")]);
    // A complete but filtered-out final record without its newline.
    append(&path, &policy);
    let query = || LogQuery {
        limit: Some(10),
        target: Some("orbit.job".to_string()),
        ..LogQuery::default()
    };
    let filters =
        LogFilters::from_query_parts(Some("orbit.job".to_string()), None, None).expect("filters");

    let unterminated = snapshot(&path, query());
    assert_eq!(unterminated.events.len(), 1);
    assert!(unterminated.events[0].message_html.contains("kept"));
    assert_eq!(unterminated.offset, file_len(&path));

    append(&path, "\n");
    let records_end = file_len(&path);
    let partial = log_line("late");
    let (head, rest) = partial.split_at(partial.len() / 2);
    append(&path, head);
    let torn = snapshot(&path, query());
    assert_eq!(torn.events.len(), 1);
    assert_eq!(torn.offset, records_end);

    let frames = stream_frames_until_sentinel(&path, filters, torn.offset, || {
        append(&path, &format!("{rest}\n"));
    });
    assert_eq!(delivered(&torn, &frames, "kept"), 1, "{frames:?}");
    assert_eq!(delivered(&torn, &frames, "late"), 1, "{frames:?}");
    assert!(
        frames.iter().all(|frame| !frame.contains("policy")),
        "{frames:?}"
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

#[test]
fn poll_backoff_grows_while_idle_is_capped_and_resets_on_progress() {
    let mut backoff = PollBackoff::new();
    let mut previous = Duration::ZERO;
    let mut capped = Duration::ZERO;
    for _ in 0..16 {
        let delay = backoff.next_delay(false);
        assert!(delay >= previous, "idle delay must never shrink");
        assert!(
            delay <= Duration::from_secs(1),
            "idle delay {delay:?} exceeded the 1s ceiling"
        );
        previous = delay;
        capped = delay;
    }
    assert_eq!(
        capped,
        Duration::from_secs(1),
        "idle polling reaches the cap"
    );

    let floor = backoff.next_delay(true);
    assert!(
        floor <= Duration::from_millis(50),
        "new data resets to the fast interval, got {floor:?}"
    );
    assert!(
        backoff.next_delay(false) > floor,
        "backoff resumes after a reset"
    );
}

/// A resume offset past the end of the file is clamped to the end: the
/// stream must not replay the existing log, but must still deliver a line
/// appended afterwards.
#[test]
fn stream_resume_past_the_end_replays_the_rotated_file() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    // The client last saw an offset in the pre-rotation file; the log rotated
    // while its stream was closed, so the current file is shorter.
    write_lines(&path, &[log_line("new-1"), log_line("new-2")]);

    let frames = stream_frames_until_sentinel(&path, LogFilters::default(), u64::MAX, || {});
    assert_eq!(
        frames.len(),
        2,
        "a resume offset past the end must replay the rotated file, not skip it: {frames:?}"
    );
    assert!(frames[0].contains("new-1") && frames[1].contains("new-2"));
}

/// A file that shrinks while a stream is open (rotation) restarts from its
/// beginning, so the new file's lines are not lost.
#[test]
fn stream_restarts_from_zero_when_the_open_file_shrinks() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("orbit.jsonl");
    write_lines(
        &path,
        &[
            log_line("before-1"),
            log_line("before-2"),
            log_line("before-3"),
        ],
    );
    let gate = LogStreamGate::new(1);
    let permit = gate.try_acquire().expect("stream permit");
    let mut rx = spawn_log_sse_frames(path.clone(), LogFilters::default(), permit, Some(0));
    // The replay proves the stream thread is running and positioned at the
    // old end before the file is swapped.
    let replay = collect_sse_frames(&mut rx, 3);
    assert_eq!(replay.len(), 3, "the pre-rotation lines replay");

    write_lines(&path, &[log_line("rotated")]);
    let frames = collect_sse_frames(&mut rx, 1);
    assert_eq!(frames.len(), 1, "the rotated file's line is delivered");
    assert!(frames[0].contains("rotated"), "got {}", frames[0]);
}
