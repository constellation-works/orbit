// A Health › Errors refresh must read only the log bytes inside its window.
use std::cell::Cell;
use std::fs;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use super::super::filter::{Filters, LevelFilter};
use super::super::scan::{
    VALUE_PARSES, parse_matching_event, read_recent_matching_events_across_segments_with,
};

const BLOCK: usize = 64 * 1024;

fn record(ts: DateTime<Utc>, level: &str, id: &str, message: &str) -> String {
    format!(
        "{}\n",
        json!({"timestamp": ts.to_rfc3339(), "level": level, "target": "backend",
            "fields": {"event_id": id, "message": message}})
    )
}

fn error_filters(since: DateTime<Utc>) -> Filters {
    Filters::new(None, Some(LevelFilter::Error), Some(since))
}

fn event_ids(events: &[Value]) -> Vec<&str> {
    events
        .iter()
        .map(|event| event["fields"]["event_id"].as_str().unwrap())
        .collect()
}

/// An in-memory segment that counts the bytes read from it.
struct Counting<'a> {
    inner: Cursor<&'a [u8]>,
    read: &'a Cell<u64>,
}

impl Read for Counting<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

impl Seek for Counting<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn rotated_name(active: &str, rotated_at: DateTime<Utc>) -> String {
    format!("{active}.{}", rotated_at.format("%Y%m%dT%H%M%S%3fZ"))
}

/// A ~100 MB rotated segment that starts before the window and ends inside
/// it is read only back to the window's first record, and the result is what
/// a full read of the segment gives.
#[test]
fn a_segment_straddling_the_window_is_read_only_back_to_its_start() {
    let now = Utc::now();
    let since = now - Duration::hours(24);

    // 96 MiB written well before the window, errors included.
    let stale_ts = since - Duration::hours(3);
    let mut chunk = String::new();
    while chunk.len() < 1 << 20 {
        chunk.push_str(&record(stale_ts, "ERROR", "stale", "stale failure"));
        chunk.push_str(&record(stale_ts, "INFO", "stale-info", "stale note"));
    }
    let mut archive = Vec::new();
    for _ in 0..96 {
        archive.extend_from_slice(chunk.as_bytes());
    }
    let stale_len = archive.len();
    let mut expected = Vec::new();
    for index in 0..4_000 {
        let ts = since + Duration::minutes(5) + Duration::seconds(index);
        if index % 8 == 0 {
            let id = format!("archive-error-{index}");
            archive.extend_from_slice(record(ts, "ERROR", &id, "failed").as_bytes());
            expected.push(id);
        } else {
            archive.extend_from_slice(record(ts, "INFO", "info", "fine").as_bytes());
        }
    }
    let in_window = (archive.len() - stale_len) as u64;
    let longest_line = record(stale_ts, "ERROR", "stale", "stale failure").len() as u64;
    assert!(archive.len() > 96 << 20);

    let active_bytes = record(now - Duration::minutes(1), "ERROR", "active-error", "boom");
    expected.push("active-error".to_string());

    let dir = tempfile::tempdir().unwrap();
    let active = dir.path().join("process.log");
    let archive_path = dir.path().join(rotated_name("process.log", now));
    fs::write(&active, "").unwrap();
    fs::write(&archive_path, "").unwrap();

    let archive_read = Cell::new(0);
    let active_read = Cell::new(0);
    let scanned = read_recent_matching_events_across_segments_with(
        &active,
        &error_filters(since),
        50_000,
        BLOCK,
        &mut |path| {
            let (bytes, read) = if path == active {
                (active_bytes.as_bytes(), &active_read)
            } else {
                (archive.as_slice(), &archive_read)
            };
            Ok(Some(Counting {
                inner: Cursor::new(bytes),
                read,
            }))
        },
    )
    .unwrap();

    assert_eq!(event_ids(&scanned.events), expected);
    assert_eq!(
        scanned.coverage_since, None,
        "the segment starts before the window, so coverage is complete"
    );
    assert!(
        archive_read.get() <= in_window + longest_line + BLOCK as u64,
        "read {} bytes of the archive for {in_window} in-window bytes",
        archive_read.get()
    );
    assert!(archive_read.get() >= in_window, "the window was not read");
}

/// A segment last written before the window is not opened, and neither is
/// any older one; the segment before it is still read to its start.
#[test]
fn segments_last_written_before_the_window_are_not_opened() {
    let now = Utc::now();
    let since = now - Duration::hours(24);
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, records: &[String], modified: DateTime<Utc>| {
        let path = dir.path().join(name);
        fs::write(&path, records.concat()).unwrap();
        let modified = SystemTime::from(modified);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    };
    let active = write(
        "process.log",
        &[record(now, "ERROR", "active", "boom")],
        now,
    );
    let in_window = write(
        &rotated_name("process.log", now - Duration::hours(1)),
        &[record(
            since + Duration::hours(1),
            "ERROR",
            "rotated",
            "boom",
        )],
        now - Duration::hours(1),
    );
    // Each holds an in-window-looking error that must not be read.
    let before = since - Duration::hours(2);
    write(
        &rotated_name("process.log", since - Duration::hours(1)),
        &[record(before, "ERROR", "before", "boom")],
        since - Duration::hours(1),
    );
    write(
        &rotated_name("process.log", since - Duration::hours(30)),
        &[record(before, "ERROR", "oldest", "boom")],
        since - Duration::hours(30),
    );

    let opened: std::cell::RefCell<Vec<PathBuf>> = Default::default();
    let scanned = read_recent_matching_events_across_segments_with(
        &active,
        &error_filters(since),
        50_000,
        BLOCK,
        &mut |path: &Path| {
            opened.borrow_mut().push(path.to_path_buf());
            fs::File::open(path).map(Some)
        },
    )
    .unwrap();

    assert_eq!(event_ids(&scanned.events), ["rotated", "active"]);
    assert_eq!(opened.into_inner(), [active, in_window]);
    assert_eq!(scanned.coverage_since, None);
}

/// Lines that are outside the window or below ERROR are rejected from their
/// header: no `Value` is built for them, even when they mention "error".
#[test]
fn rejected_lines_build_no_value() {
    let now = Utc::now();
    let since = now - Duration::hours(24);
    let mut segment = String::new();
    for _ in 0..1_000 {
        segment.push_str(&record(
            since - Duration::hours(2),
            "ERROR",
            "old",
            "old error",
        ));
    }
    for index in 0..100_000 {
        let message = if index % 2 == 0 {
            "agent relayed an error line"
        } else {
            "all quiet"
        };
        let ts = since + Duration::minutes(1) + Duration::milliseconds(index);
        segment.push_str(&record(ts, "INFO", "info", message));
    }
    segment.push_str(&record(now, "ERROR", "the-error", "boom"));

    let dir = tempfile::tempdir().unwrap();
    let active = dir.path().join("process.log");
    fs::write(&active, "").unwrap();
    VALUE_PARSES.with(|parses| parses.set(0));
    let scanned = read_recent_matching_events_across_segments_with(
        &active,
        &error_filters(since),
        50_000,
        BLOCK,
        &mut |_| Ok(Some(Cursor::new(segment.as_bytes()))),
    )
    .unwrap();

    assert_eq!(event_ids(&scanned.events), ["the-error"]);
    assert_eq!(
        VALUE_PARSES.with(Cell::get),
        1,
        "only the matching record may be parsed into a Value"
    );
}

/// Combinatorial: the header-based verdict equals parsing the whole line and
/// asking `Filters::matches`, including for lines the header cannot read.
#[test]
fn line_verdicts_equal_a_full_parse() {
    let since = DateTime::parse_from_rfc3339("2026-10-07T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let lines = [
        r#"{"timestamp":"2026-10-07T13:00:00Z","level":"ERROR","target":"a::b","fields":{}}"#,
        r#"{"timestamp":"2026-10-07T13:00:00Z","level":"error","target":"a::b"}"#,
        r#"{"timestamp":"2026-10-07T13:00:00Z","level":"WARN","target":"a::b"}"#,
        r#"{"timestamp":"2026-10-07T11:00:00Z","level":"ERROR","target":"a::b"}"#,
        r#"{"timestamp":"2026-10-07T11:59:30Z","level":"ERROR","target":"a::b"}"#,
        r#"{"timestamp":"not a time","level":"ERROR","target":"a::b"}"#,
        r#"{"timestamp":17,"level":"ERROR","target":"a::b"}"#,
        r#"{"timestamp":null,"level":"ERROR"}"#,
        r#"{"level":"ERROR","fields":{"timestamp":"2026-01-01T00:00:00Z","level":"INFO"}}"#,
        r#"{"level":"ERROR","level":"INFO","timestamp":"2026-10-07T13:00:00Z"}"#,
        r#"{"level":5,"target":"a::b","timestamp":"2026-10-07T13:00:00Z"}"#,
        r#"{"level":"ERROR","target":["a"],"timestamp":"2026-10-07T13:00:00Z"}"#,
        r#"["2026-01-01T00:00:00Z","ERROR"]"#,
        r#"  {"level":"ERROR","timestamp":"2026-10-07T13:00:00Z"} "#,
        r#"{"level":"ERROR","timestamp":"2026-10-07T13:00:00Z"} trailing"#,
        r#"{"level":"ERROR""#,
        r#"7"#,
        "",
    ];
    for filters in [
        Filters::new(None, None, None),
        Filters::new(None, Some(LevelFilter::Error), None),
        Filters::new(None, Some(LevelFilter::Error), Some(since)),
        Filters::new(None, None, Some(since)),
        Filters::new(Some("a::".into()), Some(LevelFilter::Warn), Some(since)),
    ] {
        for line in lines {
            let reference = serde_json::from_str::<Value>(line)
                .ok()
                .filter(|value| filters.matches(value));
            assert_eq!(
                parse_matching_event(line, &filters),
                reference,
                "line {line:?} under {filters:?}"
            );
        }
    }
}
