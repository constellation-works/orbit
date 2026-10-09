//! Bounded reverse scans, rotated segments and snapshot tails.

use chrono::{DateTime, Utc};
use orbit_common::fs::reverse_lines::{REVERSE_READ_BLOCK, ReverseLines};
use orbit_core::OrbitError;
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

use super::MAX_LOG_RECORD_BYTES;
use super::filter::{Filters, LevelFilter, event_timestamp, parse_timestamp};
use super::render::{RenderedLogEvent, render_log_event_for_web};

pub(crate) fn resolve_log_path(override_path: Option<&Path>) -> Result<PathBuf, OrbitError> {
    if let Some(path) = override_path {
        return Ok(path.to_path_buf());
    }
    if let Ok(env) = std::env::var("ORBIT_LOG_PATH")
        && !env.is_empty()
    {
        return Ok(PathBuf::from(env));
    }
    orbit_common::observability::logging::global_jsonl_log_path().map_err(|err| {
        OrbitError::InvalidInput(format!("cannot resolve global JSONL log path: {err}"))
    })
}

/// Block size for reverse JSONL scans. A scan stops at `limit` matches or at the
/// first record older than its window, and only reaches offset 0 without either.
const TAIL_READ_BLOCK: usize = REVERSE_READ_BLOCK;

/// Newest matching events across the active log and its rotated archives,
/// oldest first, plus the instant the scan's coverage starts when that is
/// later than the filter's `since` (or when there is no `since`).
#[derive(Debug, Default)]
pub(crate) struct SegmentedEvents {
    pub events: Vec<Value>,
    pub coverage_since: Option<DateTime<Utc>>,
}

/// Newest matching events over `active` and then its rotated archives, newest
/// segment first, stopping once `limit` events are in hand or a segment starts
/// at or before the filter's `since`.
///
/// Only bytes that can hold an in-window record are read: a segment whose last
/// write precedes the window is not opened (nor are the older ones), and a
/// segment's walk ends at the first record older than the window.
///
/// Coverage is bounded by retention (the oldest retained segment starts after
/// `since`) or by `limit` (the scan stopped at the oldest event it kept).
pub(crate) fn read_recent_matching_events_across_segments(
    active: &Path,
    filters: &Filters,
    limit: usize,
) -> io::Result<SegmentedEvents> {
    read_recent_matching_events_across_segments_with(
        active,
        filters,
        limit,
        TAIL_READ_BLOCK,
        &mut |segment| match File::open(segment) {
            Ok(file) => Ok(Some(file)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        },
    )
}

/// [`read_recent_matching_events_across_segments`] reading each segment through
/// `open` (`None` for a missing one) in `block_size` blocks, so a test can count
/// the segments opened and the bytes read.
pub(crate) fn read_recent_matching_events_across_segments_with<R: Read + Seek>(
    active: &Path,
    filters: &Filters,
    limit: usize,
    block_size: usize,
    open: &mut dyn FnMut(&Path) -> io::Result<Option<R>>,
) -> io::Result<SegmentedEvents> {
    if limit == 0 {
        return Ok(SegmentedEvents::default());
    }
    let floor = filters.window_floor();
    let mut newest_first = Vec::new();
    let mut coverage_since = None;
    for segment in log_segments_newest_first(active)? {
        if floor.is_some_and(|floor| last_written_before(&segment, floor)) {
            // Every record is older than the window, as is every older segment.
            coverage_since = None;
            break;
        }
        let Some(mut reader) = open(&segment)? else {
            continue;
        };
        let remaining = limit - newest_first.len();
        let scan = scan_recent_matching_events(&mut reader, filters, remaining, block_size)?;
        let full = scan.events.len() == remaining;
        newest_first.extend(scan.events.into_iter().rev());
        if full {
            coverage_since = newest_first.last().and_then(event_timestamp);
            break;
        }
        if scan.reached_window_start {
            // The segment holds a record older than the window, so it starts
            // before it.
            coverage_since = None;
            break;
        }
        let Some(start) = first_event_timestamp(&mut reader)? else {
            continue;
        };
        if filters.since.is_some_and(|since| start <= since) {
            coverage_since = None;
            break;
        }
        coverage_since = Some(start);
    }
    newest_first.reverse();
    Ok(SegmentedEvents {
        events: newest_first,
        coverage_since,
    })
}

/// Whether the file at `path` was last written before `instant`. No record is
/// stamped after its file's last write, so such a file holds nothing newer. A
/// file whose time cannot be read is not skipped.
fn last_written_before(path: &Path, instant: DateTime<Utc>) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| DateTime::<Utc>::from(modified) < instant)
}

/// The active log followed by its rotated archives, newest first. Archives are
/// the active file's siblings named `<active>.<UTC stamp>` by
/// `orbit_common::observability::log_rotation`; other siblings are ignored.
fn log_segments_newest_first(active: &Path) -> io::Result<Vec<PathBuf>> {
    let mut segments = vec![active.to_path_buf()];
    let (Some(dir), Some(name)) = (
        active.parent(),
        active.file_name().and_then(|name| name.to_str()),
    ) else {
        return Ok(segments);
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{name}.");
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(segments),
        Err(err) => return Err(err),
    };
    let mut archives = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(stamp) = file_name
            .to_str()
            .and_then(|file_name| file_name.strip_prefix(&prefix))
        else {
            continue;
        };
        if let Ok(rotated_at) = chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%dT%H%M%S%3fZ") {
            archives.push((rotated_at, entry.path()));
        }
    }
    archives.sort_by_key(|(rotated_at, _)| std::cmp::Reverse(*rotated_at));
    segments.extend(archives.into_iter().map(|(_, path)| path));
    Ok(segments)
}

/// Lines a segment's start is looked for in before it counts as unknown.
const SEGMENT_START_LINES: usize = 64;

/// Timestamp of the first timestamped record in a segment, read forward from
/// its start with a bounded budget; `None` for an empty or unreadable-start
/// segment.
fn first_event_timestamp<R: Read + Seek>(reader: &mut R) -> io::Result<Option<DateTime<Utc>>> {
    use std::io::BufRead;

    reader.seek(SeekFrom::Start(0))?;
    let budget = (MAX_LOG_RECORD_BYTES as u64).saturating_mul(4);
    let mut reader = io::BufReader::new(reader.by_ref().take(budget));
    let mut line = Vec::new();
    for _ in 0..SEGMENT_START_LINES {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if let Some(ts) = LineHead::parse(&line).and_then(|head| head.timestamp()) {
            return Ok(Some(ts));
        }
    }
    Ok(None)
}

/// The fields of a record that decide whether it is wanted, read without
/// building a [`Value`]: serde skips everything else (an agent line's payload
/// can be large) without allocating.
#[derive(Deserialize)]
struct LineHead<'a> {
    #[serde(borrow)]
    timestamp: Option<Cow<'a, str>>,
    #[serde(borrow)]
    level: Option<Cow<'a, str>>,
    #[serde(borrow)]
    target: Option<Cow<'a, str>>,
}

impl<'a> LineHead<'a> {
    /// `None` unless `raw` is a JSON object whose three fields are strings or
    /// absent. Anything else (a duplicate key, a number where a string is
    /// expected, an array) is left to the full [`Value`] parse so it is judged
    /// exactly as [`Filters::matches`] judges it.
    fn parse(raw: &'a [u8]) -> Option<Self> {
        let first = raw.iter().find(|byte| !byte.is_ascii_whitespace())?;
        if *first != b'{' {
            return None;
        }
        serde_json::from_slice(raw).ok()
    }

    fn timestamp(&self) -> Option<DateTime<Utc>> {
        self.timestamp.as_deref().and_then(parse_timestamp)
    }
}

#[cfg(test)]
thread_local! {
    /// Lines [`parse_value`] has built a [`Value`] for on this thread.
    pub(crate) static VALUE_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn parse_value(raw: &str) -> Option<Value> {
    #[cfg(test)]
    VALUE_PARSES.with(|parses| parses.set(parses.get() + 1));
    serde_json::from_str(raw).ok()
}

/// The verdict on one log line.
enum Scanned {
    Match(Value),
    /// Well formed or not, not wanted.
    Skip,
    /// Stamped before the filter's window, so the rest of an append-ordered
    /// file read backwards is too.
    BeforeWindow,
}

/// Judge `raw` against `filters`. A line that is not wanted is rejected from
/// its timestamp, level and target alone; only a match builds a [`Value`].
fn scan_line(raw: &str, filters: &Filters) -> Scanned {
    // Without a window there is no boundary to find, so a line that never
    // mentions the level it needs is skipped unread.
    if filters.since.is_none()
        && filters.min_level == Some(LevelFilter::Error)
        && !raw
            .as_bytes()
            .windows(5)
            .any(|window| window.eq_ignore_ascii_case(b"error"))
    {
        return Scanned::Skip;
    }
    let Some(head) = LineHead::parse(raw.as_bytes()) else {
        let Some(value) = parse_value(raw) else {
            return Scanned::Skip;
        };
        if filters.precedes_window(event_timestamp(&value)) {
            return Scanned::BeforeWindow;
        }
        return if filters.matches(&value) {
            Scanned::Match(value)
        } else {
            Scanned::Skip
        };
    };
    let ts = head.timestamp();
    if filters.precedes_window(ts) {
        return Scanned::BeforeWindow;
    }
    let wanted = filters.matches_parts(
        head.target.as_deref().unwrap_or(""),
        head.level.as_deref(),
        ts,
    );
    match wanted.then(|| parse_value(raw)).flatten() {
        Some(value) => Scanned::Match(value),
        None => Scanned::Skip,
    }
}

/// Newest matching JSONL events from a seekable reader, scanning backwards.
///
/// Malformed JSON and non-UTF-8 lines are skipped; I/O errors propagate.
/// `block_size` is the read window. Callers use [`TAIL_READ_BLOCK`].
fn read_recent_matching_events_from<R: Read + Seek>(
    reader: R,
    filters: &Filters,
    limit: usize,
    block_size: usize,
) -> io::Result<Vec<Value>> {
    Ok(scan_recent_matching_events(reader, filters, limit, block_size)?.events)
}

/// Matching events of a backward scan, oldest first.
struct RecentScan {
    events: Vec<Value>,
    /// The scan stopped at a record older than the filter's window rather than
    /// at `limit` or the start of the reader.
    reached_window_start: bool,
}

/// [`read_recent_matching_events_from`], also saying whether the walk ended at
/// the window's start. It reads no further back than the first record older
/// than the window (see [`Filters::window_floor`]).
fn scan_recent_matching_events<R: Read + Seek>(
    reader: R,
    filters: &Filters,
    limit: usize,
    block_size: usize,
) -> io::Result<RecentScan> {
    if limit == 0 {
        return Ok(RecentScan {
            events: Vec::new(),
            reached_window_start: false,
        });
    }
    // `limit` ultimately originates at the request boundary. Grow this vector
    // only as matching records are found rather than preallocating from it.
    let mut newest_first = Vec::new();
    let mut reached_window_start = false;
    for line in ReverseLines::with_block_size(reader, block_size)? {
        let line = match line {
            Ok(line) => line,
            // Only skip decoding failures, not an InvalidData from the reader.
            Err(err)
                if err
                    .get_ref()
                    .is_some_and(|cause| cause.is::<FromUtf8Error>()) =>
            {
                continue;
            }
            Err(err) => return Err(err),
        };
        match scan_line(&line, filters) {
            Scanned::Match(event) => {
                newest_first.push(event);
                if newest_first.len() == limit {
                    break;
                }
            }
            Scanned::Skip => {}
            Scanned::BeforeWindow => {
                reached_window_start = true;
                break;
            }
        }
    }
    newest_first.reverse();
    Ok(RecentScan {
        events: newest_first,
        reached_window_start,
    })
}

/// Newest matching events of a snapshot plus the byte cursor that resumes
/// exactly after the bytes those events were drawn from.
#[derive(Debug)]
pub(crate) struct RenderedLogTail {
    pub events: Vec<RenderedLogEvent>,
    pub cursor: u64,
}

/// Snapshot the newest matching events of the log at `path` together with a
/// replay cursor bound to the same file extent.
///
/// The extent is fixed by one `stat` of the open handle; bytes appended later
/// are neither scanned nor covered by the cursor, so a stream resumed at
/// `cursor` delivers each record exactly once across snapshot and stream.
pub(crate) fn read_recent_rendered_tail(
    path: &Path,
    filters: &Filters,
    limit: usize,
) -> io::Result<RenderedLogTail> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(RenderedLogTail {
                events: Vec::new(),
                cursor: 0,
            });
        }
        Err(err) => return Err(err),
    };
    let len = file.metadata()?.len();
    read_rendered_tail_from(&mut file, len, filters, limit, TAIL_READ_BLOCK)
}

/// [`read_recent_rendered_tail`] over the first `len` bytes of `reader`.
///
/// Newline-terminated records are always complete. An unterminated final
/// record counts only when it parses as JSON: a finished record written
/// without a newline is served and the cursor moves past it (its newline later
/// reads as an empty, skipped line), while a partial write leaves the cursor at
/// its start so the stream reads it whole once it completes.
fn read_rendered_tail_from<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    filters: &Filters,
    limit: usize,
    block_size: usize,
) -> io::Result<RenderedLogTail> {
    let block_size = if block_size == 0 {
        TAIL_READ_BLOCK
    } else {
        block_size
    };
    let records_end = complete_records_end(reader, len, block_size)?;
    // An unterminated tail longer than the stream's record cap is treated like
    // a partial write (not served, cursor left at its start) without being
    // read: the file's tail is untrusted length, so never allocate from it.
    let final_record = match usize::try_from(len - records_end) {
        Ok(tail_len) if tail_len <= MAX_LOG_RECORD_BYTES => {
            let mut unterminated = vec![0; tail_len];
            reader.seek(SeekFrom::Start(records_end))?;
            reader.read_exact(&mut unterminated)?;
            std::str::from_utf8(&unterminated)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        }
        _ => None,
    };
    let cursor = if final_record.is_some() {
        len
    } else {
        records_end
    };

    let final_match = final_record.filter(|event| limit > 0 && filters.matches(event));
    let earlier_limit = limit - usize::from(final_match.is_some());
    let mut events = read_recent_matching_events_from(
        Prefix::new(reader, records_end),
        filters,
        earlier_limit,
        block_size,
    )?;
    events.extend(final_match);
    Ok(RenderedLogTail {
        events: events.iter().map(render_log_event_for_web).collect(),
        cursor,
    })
}

/// Offset just past the last newline within the first `len` bytes, or 0.
fn complete_records_end<R: Read + Seek>(
    reader: &mut R,
    len: u64,
    block_size: usize,
) -> io::Result<u64> {
    let mut block = vec![0; block_size];
    let mut end = len;
    while end > 0 {
        let start = end.saturating_sub(block_size as u64);
        let window = &mut block[..(end - start) as usize];
        reader.seek(SeekFrom::Start(start))?;
        reader.read_exact(window)?;
        if let Some(newline) = window.iter().rposition(|&byte| byte == b'\n') {
            return Ok(start + newline as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// The first `len` bytes of a reader, so a reverse scan starts at a fixed
/// extent instead of whatever the file has grown to.
struct Prefix<R> {
    inner: R,
    len: u64,
    pos: u64,
}

impl<R> Prefix<R> {
    fn new(inner: R, len: u64) -> Self {
        Self { inner, len, pos: 0 }
    }
}

impl<R: Read + Seek> Read for Prefix<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.len.saturating_sub(self.pos);
        let max = usize::try_from(remaining).map_or(buf.len(), |r| r.min(buf.len()));
        let n = self.inner.read(&mut buf[..max])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for Prefix<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let target = match pos {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?;
        self.pos = self.inner.seek(SeekFrom::Start(target))?;
        Ok(self.pos)
    }
}

pub(crate) fn parse_matching_event(raw: &str, filters: &Filters) -> Option<Value> {
    match scan_line(raw, filters) {
        Scanned::Match(value) => Some(value),
        Scanned::Skip | Scanned::BeforeWindow => None,
    }
}
