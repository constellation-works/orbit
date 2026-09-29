use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::time::Instant;

use orbit_common::test_fixtures::TEST_CODEX_MODEL;
use serde_json::{Value, json};

use super::super::log_format::*;

#[test]
fn format_message_html_escapes_dynamic_field_values() {
    let html = format_message_html(
        "orbit.friction.reported",
        &json!({
            "task_id": "<script>alert(1)</script>",
            "agent": "codex",
            "model": TEST_CODEX_MODEL,
            "summary": "bad <b>markup</b>"
        }),
    );

    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(html.contains("bad &lt;b&gt;markup&lt;/b&gt;"));
    assert!(!html.contains("<script>"));
}

#[test]
fn render_log_event_for_web_uses_shared_labels_and_lowercase_level() {
    let event = json!({
        "timestamp": "2026-04-27T01:00:03.000000000Z",
        "level": "WARN",
        "target": "orbit.policy.deny",
        "fields": {
            "tool": "fs.write",
            "path": "/etc/passwd",
            "profile": "writer",
            "matched_rule": "/etc/**"
        }
    });

    let rendered = render_log_event_for_web(&event);
    assert_eq!(rendered.ts, "2026-04-27T01:00:03.000000000Z");
    assert_eq!(rendered.source, "policy");
    assert_eq!(rendered.code, "DENY");
    assert_eq!(rendered.level, "warn");
    assert!(rendered.message_html.contains("<b>path</b>="));
}

fn event_line(ts: &str, target: &str, message: &str) -> String {
    json!({
        "timestamp": ts,
        "level": "INFO",
        "target": target,
        "fields": { "message": message }
    })
    .to_string()
}

fn join_lines(lines: &[String], trailing_newline: bool) -> String {
    let mut raw = lines.join("\n");
    if trailing_newline && !raw.is_empty() {
        raw.push('\n');
    }
    raw
}

fn naive_forward(raw: &str, filters: &Filters, limit: usize) -> Vec<Value> {
    let mut kept = Vec::new();
    for line in raw.lines() {
        if let Some(event) = parse_matching_event(line, filters) {
            kept.push(event);
            if kept.len() > limit {
                kept.remove(0);
            }
        }
    }
    kept
}

fn scan(raw: &str, filters: &Filters, limit: usize, block_size: usize) -> Vec<Value> {
    read_recent_matching_events_from(Cursor::new(raw.as_bytes()), filters, limit, block_size)
        .expect("scan")
}

struct CountingReader<R> {
    inner: R,
    bytes_read: u64,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn reverse_scan_keeps_last_matches_in_chronological_order() {
    let lines = [
        event_line("2026-04-27T01:00:01Z", "orbit.keep", "a"),
        event_line("2026-04-27T01:00:02Z", "orbit.keep", "b"),
        event_line("2026-04-27T01:00:03Z", "orbit.keep", "c"),
        event_line("2026-04-27T01:00:04Z", "orbit.keep", "d"),
        event_line("2026-04-27T01:00:05Z", "orbit.keep", "e"),
    ];
    let raw = join_lines(&lines, true);
    let events = scan(&raw, &Filters::default(), 2, 16);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["fields"]["message"], "d");
    assert_eq!(events[1]["fields"]["message"], "e");
}

#[test]
fn reverse_scan_applies_target_level_and_since_filters() {
    let raw = join_lines(
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
        true,
    );
    let filters = Filters::from_query_parts(
        Some("orbit.policy".to_string()),
        Some("warn".to_string()),
        Some("2026-04-27T01:00:02Z"),
    )
    .expect("filters");
    let events = scan(&raw, &filters, 10, 32);
    assert_eq!(events, naive_forward(&raw, &filters, 10));
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["level"], "WARN");
    assert_eq!(events[0]["target"], "orbit.policy.deny");
}

#[test]
fn reverse_scan_skips_malformed_records() {
    let raw = format!(
        "{}\nnot-json\n{}\n{}\n",
        event_line("2026-04-27T01:00:01Z", "orbit.keep", "first"),
        "{",
        event_line("2026-04-27T01:00:02Z", "orbit.keep", "second"),
    );
    let events = scan(&raw, &Filters::default(), 10, 8);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["fields"]["message"], "first");
    assert_eq!(events[1]["fields"]["message"], "second");
}

#[test]
fn reverse_scan_reconstructs_lines_that_span_blocks() {
    let long_message = "m".repeat(40);
    let lines = [
        event_line("2026-04-27T01:00:01Z", "orbit.keep", "early"),
        event_line("2026-04-27T01:00:02Z", "orbit.keep", &long_message),
        event_line("2026-04-27T01:00:03Z", "orbit.keep", "late"),
    ];
    let raw = join_lines(&lines, true);
    assert!(
        lines[1].len() > 16,
        "fixture line must exceed the test block size"
    );
    let events = scan(&raw, &Filters::default(), 10, 16);
    assert_eq!(events, naive_forward(&raw, &Filters::default(), 10));
    assert_eq!(events.len(), 3);
    assert_eq!(events[1]["fields"]["message"], long_message);
}

#[test]
fn reverse_scan_includes_final_line_without_trailing_newline() {
    let lines = [
        event_line("2026-04-27T01:00:01Z", "orbit.keep", "a"),
        event_line("2026-04-27T01:00:02Z", "orbit.keep", "b"),
    ];
    let raw = join_lines(&lines, false);
    assert!(!raw.ends_with('\n'));
    let events = scan(&raw, &Filters::default(), 10, 16);
    assert_eq!(events, naive_forward(&raw, &Filters::default(), 10));
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["fields"]["message"], "b");
}

#[test]
fn reverse_scan_zero_limit_returns_empty_without_reading() {
    let raw = join_lines(
        &[event_line("2026-04-27T01:00:01Z", "orbit.keep", "a")],
        true,
    );
    let mut reader = CountingReader {
        inner: Cursor::new(raw.as_bytes()),
        bytes_read: 0,
    };
    let events = read_recent_matching_events_from(&mut reader, &Filters::default(), 0, 16)
        .expect("zero limit");
    assert!(events.is_empty());
    assert_eq!(reader.bytes_read, 0);
}

#[test]
fn reverse_scan_does_not_preallocate_from_requested_limit() {
    let raw = join_lines(
        &[event_line("2026-04-27T01:00:01Z", "orbit.keep", "only")],
        true,
    );

    let events = scan(&raw, &Filters::default(), usize::MAX, 16);

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["fields"]["message"], "only");
}

#[test]
fn missing_log_file_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("missing.jsonl");
    let events = read_recent_matching_events(&path, &Filters::default(), 50).expect("missing");
    assert!(events.is_empty());
}

#[test]
fn dense_tail_does_not_read_or_parse_unnecessary_prefix() {
    let mut lines = Vec::new();
    for i in 0..2000 {
        lines.push(event_line(
            "2026-04-27T01:00:00Z",
            "orbit.noise",
            &format!("n{i:04}"),
        ));
    }
    for i in 0..50 {
        lines.push(event_line(
            "2026-04-27T02:00:00Z",
            "orbit.keep",
            &format!("k{i:02}"),
        ));
    }
    let raw = join_lines(&lines, true);
    let prefix_len = {
        let last_noise = lines[1999].as_str();
        raw.find(last_noise).expect("prefix") + last_noise.len() + 1
    };
    let filters =
        Filters::from_query_parts(Some("orbit.keep".to_string()), None, None).expect("filters");
    let mut reader = CountingReader {
        inner: Cursor::new(raw.as_bytes()),
        bytes_read: 0,
    };
    let events =
        read_recent_matching_events_from(&mut reader, &filters, 50, TAIL_READ_BLOCK).expect("tail");

    assert_eq!(events.len(), 50);
    assert_eq!(events[0]["fields"]["message"], "k00");
    assert_eq!(events[49]["fields"]["message"], "k49");
    assert_eq!(events, naive_forward(&raw, &filters, 50));
    assert!(
        reader.bytes_read < prefix_len as u64,
        "dense tail must not read the unmatched prefix: read {} of {} prefix bytes (file {})",
        reader.bytes_read,
        prefix_len,
        raw.len()
    );
}

#[test]
fn sparse_filter_scans_to_file_start_and_returns_available_matches() {
    let mut lines = vec![
        event_line("2026-04-27T01:00:01Z", "orbit.keep", "early-a"),
        event_line("2026-04-27T01:00:02Z", "orbit.keep", "early-b"),
        event_line("2026-04-27T01:00:03Z", "orbit.keep", "early-c"),
    ];
    for i in 0..1500 {
        lines.push(event_line(
            "2026-04-27T02:00:00Z",
            "orbit.noise",
            &format!("n{i:04}"),
        ));
    }
    let raw = join_lines(&lines, true);
    let filters =
        Filters::from_query_parts(Some("orbit.keep".to_string()), None, None).expect("filters");
    let mut reader = CountingReader {
        inner: Cursor::new(raw.as_bytes()),
        bytes_read: 0,
    };
    let events = read_recent_matching_events_from(&mut reader, &filters, 50, TAIL_READ_BLOCK)
        .expect("sparse");

    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["fields"]["message"], "early-a");
    assert_eq!(events[2]["fields"]["message"], "early-c");
    assert_eq!(events, naive_forward(&raw, &filters, 50));
    assert_eq!(
        reader.bytes_read,
        raw.len() as u64,
        "sparse matches at file start must scan the whole fixture"
    );
}

#[test]
#[allow(clippy::print_stdout)]
fn reverse_scan_measurement_records_work_not_wall_clock() {
    let noise = 4000usize;
    let matches = 50usize;
    let mut lines = Vec::with_capacity(noise + matches);
    for i in 0..noise {
        lines.push(event_line(
            "2026-04-27T01:00:00Z",
            "orbit.noise",
            &format!("n{i:04}"),
        ));
    }
    for i in 0..matches {
        lines.push(event_line(
            "2026-04-27T02:00:00Z",
            "orbit.keep",
            &format!("k{i:02}"),
        ));
    }
    let raw = join_lines(&lines, true);
    let filters =
        Filters::from_query_parts(Some("orbit.keep".to_string()), None, None).expect("filters");
    let density = matches as f64 / lines.len() as f64;

    let naive_start = Instant::now();
    let naive = naive_forward(&raw, &filters, matches);
    let naive_ms = naive_start.elapsed().as_secs_f64() * 1000.0;

    let mut reader = CountingReader {
        inner: Cursor::new(raw.as_bytes()),
        bytes_read: 0,
    };
    let reverse_start = Instant::now();
    let events = read_recent_matching_events_from(&mut reader, &filters, matches, TAIL_READ_BLOCK)
        .expect("reverse");
    let reverse_ms = reverse_start.elapsed().as_secs_f64() * 1000.0;

    assert_eq!(events, naive);
    assert_eq!(events.len(), matches);

    println!(
        "log reverse-scan measurement: fixture_bytes={} records={} matches={} density={:.4} block={} bytes_read={} naive_ms={:.3} reverse_ms={:.3} os={} arch={}",
        raw.len(),
        lines.len(),
        matches,
        density,
        TAIL_READ_BLOCK,
        reader.bytes_read,
        naive_ms,
        reverse_ms,
        std::env::consts::OS,
        std::env::consts::ARCH
    );
}

#[test]
fn rendered_tail_ignores_bytes_past_the_scanned_extent() {
    let lines = [
        event_line("2026-04-27T01:00:01Z", "orbit.keep", "early"),
        event_line("2026-04-27T01:00:02Z", "orbit.keep", "scanned"),
    ];
    let scanned = join_lines(&lines, true);
    let appended = event_line("2026-04-27T01:00:03Z", "orbit.keep", "appended");
    let raw = format!("{scanned}{appended}\n");
    let mut reader = Cursor::new(raw.as_bytes());

    let tail = read_rendered_tail_from(
        &mut reader,
        scanned.len() as u64,
        &Filters::default(),
        10,
        16,
    )
    .expect("tail");

    assert_eq!(tail.events.len(), 2);
    assert!(tail.events[1].message_html.contains("scanned"));
    assert!(
        tail.events
            .iter()
            .all(|e| !e.message_html.contains("appended"))
    );
    assert_eq!(tail.cursor, scanned.len() as u64);
}

#[test]
fn rendered_tail_cursor_stops_before_a_partial_record() {
    let complete = join_lines(
        &[event_line("2026-04-27T01:00:01Z", "orbit.keep", "whole")],
        true,
    );
    let partial = event_line("2026-04-27T01:00:02Z", "orbit.keep", "torn");
    let raw = format!("{complete}{}", &partial[..partial.len() / 2]);
    let mut reader = Cursor::new(raw.as_bytes());

    let tail = read_rendered_tail_from(&mut reader, raw.len() as u64, &Filters::default(), 10, 8)
        .expect("tail");

    assert_eq!(tail.events.len(), 1);
    assert!(tail.events[0].message_html.contains("whole"));
    assert_eq!(tail.cursor, complete.len() as u64);
}

/// The unterminated trailing record is never read into memory past the
/// stream's record cap: it is treated like a partial write, so the tail
/// serves the complete records and leaves the cursor at the tail's start.
#[test]
fn rendered_tail_skips_an_oversized_unterminated_record() {
    let complete = join_lines(
        &[event_line("2026-04-27T01:00:01Z", "orbit.keep", "whole")],
        true,
    );
    let huge = event_line(
        "2026-04-27T01:00:02Z",
        "orbit.keep",
        &"x".repeat(MAX_LOG_RECORD_BYTES + 1),
    );
    let raw = format!("{complete}{huge}");
    let mut reader = Cursor::new(raw.as_bytes());

    let tail = read_rendered_tail_from(&mut reader, raw.len() as u64, &Filters::default(), 10, 8)
        .expect("tail");

    assert_eq!(tail.events.len(), 1, "only the complete record is served");
    assert!(tail.events[0].message_html.contains("whole"));
    assert_eq!(tail.cursor, complete.len() as u64);
}

#[test]
fn rendered_tail_still_serves_a_small_unterminated_record() {
    let record = event_line("2026-04-27T01:00:02Z", "orbit.keep", "edge");
    let mut reader = Cursor::new(record.as_bytes());

    let tail =
        read_rendered_tail_from(&mut reader, record.len() as u64, &Filters::default(), 10, 8)
            .expect("tail");

    assert_eq!(tail.events.len(), 1);
    assert_eq!(tail.cursor, record.len() as u64);
}

/// Secret-looking tokens in log fields are redacted before HTML escaping,
/// for the generic field renderer and the CLI-runner line renderer alike.
#[test]
fn rendered_log_redacts_secret_tokens_in_lines_and_fields() {
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
    let runner = render_log_event_for_web(&json!({
        "timestamp": "2026-04-27T01:00:03Z",
        "level": "INFO",
        "target": "orbit_engine::activity_job::cli_runner",
        "fields": {"stream": "stderr", "line": format!("using key {secret} now")}
    }));
    let generic = render_log_event_for_web(&json!({
        "timestamp": "2026-04-27T01:00:03Z",
        "level": "INFO",
        "target": "orbit.other",
        "fields": {"message": format!("token {secret}"), "detail": {"header": "Authorization: Bearer abc123def456"}}
    }));

    for (label, rendered) in [("cli_runner line", runner), ("generic fields", generic)] {
        assert!(
            !rendered.message_html.contains(secret)
                && !rendered.message_html.contains("abc123def456"),
            "{label} leaked a secret: {}",
            rendered.message_html
        );
        assert!(
            rendered.message_html.contains("REDACTED"),
            "{label} should show a redaction marker: {}",
            rendered.message_html
        );
    }
}
