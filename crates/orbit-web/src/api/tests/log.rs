use super::super::log::{
    LOG_STREAM_MAX_RECORD_BYTES, LogLineBuffer, LogStreamGate, read_appended_log_events,
};
use super::test_support::write_lines;
use crate::log_format::Filters as LogFilters;
use serde_json::json;
use std::io::Write;
use std::path::Path;
use tempfile::tempdir;

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

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).expect("metadata").len()
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
