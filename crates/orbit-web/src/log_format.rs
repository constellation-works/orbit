//! Web-focused subset of log tailing / rendering logic (no colored output, no
//! clap ValueEnum usage for CLI flags). Preserves exact `resolve_log_path`
//! (ORBIT_LOG_PATH + HOME fallback via orbit-common) and the HTML rendering
//! used by /api/log and /api/diagnostics.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use orbit_common::fs::reverse_lines::{REVERSE_READ_BLOCK, ReverseLines};
use orbit_common::security::redaction::redact_all;
use orbit_core::OrbitError;
use serde::Serialize;
use serde_json::Value;

use crate::parse::parse_since;

/// Longest single log record the web surfaces buffer. The SSE stream drops a
/// longer record, and the snapshot tail skips an unterminated trailing one
/// this large instead of reading it into memory.
pub(crate) const MAX_LOG_RECORD_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq, PartialOrd, Ord)]
#[clap(rename_all = "lower")]
pub(crate) enum LevelFilter {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LevelFilter {
    fn rank(self) -> u8 {
        match self {
            LevelFilter::Trace => 0,
            LevelFilter::Debug => 1,
            LevelFilter::Info => 2,
            LevelFilter::Warn => 3,
            LevelFilter::Error => 4,
        }
    }

    pub(crate) fn from_event_level(level: &str) -> Option<LevelFilter> {
        match level.to_ascii_uppercase().as_str() {
            "TRACE" => Some(LevelFilter::Trace),
            "DEBUG" => Some(LevelFilter::Debug),
            "INFO" => Some(LevelFilter::Info),
            "WARN" => Some(LevelFilter::Warn),
            "ERROR" => Some(LevelFilter::Error),
            _ => None,
        }
    }

    pub(crate) fn parse_query(raw: &str) -> Result<Self, String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "trace" => Ok(LevelFilter::Trace),
            "debug" => Ok(LevelFilter::Debug),
            "info" => Ok(LevelFilter::Info),
            "warn" | "warning" => Ok(LevelFilter::Warn),
            "error" | "err" => Ok(LevelFilter::Error),
            other => Err(format!(
                "level must be one of trace, debug, info, warn, error; got '{other}'"
            )),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Filters {
    target_prefix: Option<String>,
    min_level: Option<LevelFilter>,
    since: Option<DateTime<Utc>>,
}

impl Filters {
    pub(crate) fn new(
        target_prefix: Option<String>,
        min_level: Option<LevelFilter>,
        since: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            target_prefix,
            min_level,
            since,
        }
    }

    pub(crate) fn from_query_parts(
        target: Option<String>,
        level: Option<String>,
        since: Option<&str>,
    ) -> Result<Self, OrbitError> {
        let min_level = match level.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(raw) => Some(LevelFilter::parse_query(raw).map_err(OrbitError::InvalidInput)?),
            None => None,
        };
        let since = since.map(parse_since).transpose()?;
        Ok(Self::new(target, min_level, since))
    }

    pub(crate) fn matches(&self, event: &Value) -> bool {
        let target = event.get("target").and_then(Value::as_str).unwrap_or("");
        if let Some(prefix) = &self.target_prefix
            && !target.starts_with(prefix)
        {
            return false;
        }
        if let Some(min) = self.min_level {
            let level = event.get("level").and_then(Value::as_str).unwrap_or("INFO");
            let event_level = LevelFilter::from_event_level(level).unwrap_or(LevelFilter::Info);
            if event_level.rank() < min.rank() {
                return false;
            }
        }
        if let Some(since) = self.since
            && let Some(ts) = event.get("timestamp").and_then(Value::as_str)
            && let Ok(parsed) = DateTime::parse_from_rfc3339(ts)
            && parsed.with_timezone(&Utc) < since
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RenderedLogEvent {
    pub ts: String,
    pub source: String,
    pub target: String,
    pub code: String,
    pub level: String,
    pub message_html: String,
    pub agent_stdout: bool,
}

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

/// Block size for reverse JSONL scans. Sparse filters may still walk every
/// block to offset 0; a dense tail stops once `limit` matches are in hand.
const TAIL_READ_BLOCK: usize = REVERSE_READ_BLOCK;

pub(crate) fn read_recent_matching_events(
    path: &Path,
    filters: &Filters,
    limit: usize,
) -> io::Result<Vec<Value>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    if limit == 0 {
        return Ok(Vec::new());
    }
    read_recent_matching_events_from(file, filters, limit, TAIL_READ_BLOCK)
}

/// Newest matching events across the active log and its rotated archives,
/// oldest first, plus the instant the scan's coverage starts when that is
/// later than the filter's `since` (or when there is no `since`).
#[derive(Debug, Default)]
pub(crate) struct SegmentedEvents {
    pub events: Vec<Value>,
    pub coverage_since: Option<DateTime<Utc>>,
}

/// [`read_recent_matching_events`] over `active` and then its rotated
/// archives, newest segment first, stopping once `limit` events are in hand or
/// a segment starts at or before the filter's `since`.
///
/// Coverage is bounded by retention (the oldest retained segment starts after
/// `since`) or by `limit` (the scan stopped at the oldest event it kept).
pub(crate) fn read_recent_matching_events_across_segments(
    active: &Path,
    filters: &Filters,
    limit: usize,
) -> io::Result<SegmentedEvents> {
    if limit == 0 {
        return Ok(SegmentedEvents::default());
    }
    let mut newest_first = Vec::new();
    let mut coverage_since = None;
    for segment in log_segments_newest_first(active)? {
        let remaining = limit - newest_first.len();
        let events = read_recent_matching_events(&segment, filters, remaining)?;
        let full = events.len() == remaining;
        newest_first.extend(events.into_iter().rev());
        if full {
            coverage_since = newest_first.last().and_then(event_timestamp);
            break;
        }
        let Some(start) = first_event_timestamp(&segment)? else {
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

/// Timestamp of the first timestamped record in a segment, read forward with a
/// bounded budget; `None` for a missing, empty or unreadable-start segment.
fn first_event_timestamp(path: &Path) -> io::Result<Option<DateTime<Utc>>> {
    use std::io::BufRead;

    let file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let budget = (MAX_LOG_RECORD_BYTES as u64).saturating_mul(4);
    let mut reader = io::BufReader::new(file.take(budget));
    let mut line = Vec::new();
    for _ in 0..SEGMENT_START_LINES {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if let Some(ts) = serde_json::from_slice::<Value>(&line)
            .ok()
            .as_ref()
            .and_then(event_timestamp)
        {
            return Ok(Some(ts));
        }
    }
    Ok(None)
}

fn event_timestamp(event: &Value) -> Option<DateTime<Utc>> {
    event
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
        .map(|ts| ts.with_timezone(&Utc))
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
    if limit == 0 {
        return Ok(Vec::new());
    }
    // `limit` ultimately originates at the request boundary. Grow this vector
    // only as matching records are found rather than preallocating from it.
    let mut newest_first = Vec::new();
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
        if let Some(event) = parse_matching_event(&line, filters) {
            newest_first.push(event);
            if newest_first.len() == limit {
                break;
            }
        }
    }
    newest_first.reverse();
    Ok(newest_first)
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
    // An ERROR record names its level, so an error-only scan of rotated
    // segments skips parsing every line that never mentions it.
    if filters.min_level == Some(LevelFilter::Error)
        && !raw
            .as_bytes()
            .windows(5)
            .any(|window| window.eq_ignore_ascii_case(b"error"))
    {
        return None;
    }
    let value = serde_json::from_str::<Value>(raw).ok()?;
    filters.matches(&value).then_some(value)
}

pub(crate) fn render_log_event_for_web(event: &Value) -> RenderedLogEvent {
    let ts = event
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let level_raw = event.get("level").and_then(Value::as_str).unwrap_or("INFO");
    let target = event.get("target").and_then(Value::as_str).unwrap_or("-");
    let fields = event
        .get("fields")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));

    RenderedLogEvent {
        ts,
        source: format_source(target, &fields),
        target: redact_all(target),
        code: format_code(target, level_raw, &fields),
        level: normalize_level(level_raw).to_string(),
        message_html: format_message_html(target, &fields),
        agent_stdout: is_agent_relay(target, &fields)
            && fields.get("stream").and_then(Value::as_str) == Some("stdout"),
    }
}

fn normalize_level(level: &str) -> &'static str {
    match level.to_ascii_uppercase().as_str() {
        "TRACE" => "trace",
        "DEBUG" => "debug",
        "WARN" => "warn",
        "ERROR" => "error",
        _ => "info",
    }
}

pub(crate) fn format_source(target: &str, fields: &Value) -> String {
    if let Some(label) = match target {
        "orbit.policy.deny" => Some("policy"),
        "orbit.friction.reported" => Some("friction"),
        t if t.starts_with("orbit.job.") => Some("job"),
        _ => None,
    } {
        return label.to_string();
    }

    if target == "orbit_engine::activity_job::cli_runner"
        && let Some(provider) = fields.get("provider").and_then(Value::as_str)
    {
        return provider.to_string();
    }

    target
        .rsplit([':', '.'])
        .next()
        .unwrap_or(target)
        .to_string()
}

pub(crate) fn format_code(target: &str, level: &str, fields: &Value) -> String {
    match target {
        "orbit.policy.deny" => "DENY".to_string(),
        "orbit.friction.reported" => "FRC".to_string(),
        "orbit.job.step_retry" => "RTRY".to_string(),
        "orbit.job.step_finished" => match fields.get("success").and_then(Value::as_bool) {
            Some(true) => "OK".to_string(),
            Some(false) => "ERR".to_string(),
            None => "INF".to_string(),
        },
        _ => match level {
            "ERROR" => "ERR".to_string(),
            "WARN" => "WRN".to_string(),
            "INFO" => "INF".to_string(),
            "DEBUG" => "DBG".to_string(),
            "TRACE" => "TRC".to_string(),
            other => other.chars().take(3).collect::<String>().to_uppercase(),
        },
    }
}

/// Copy of `value` with every string scrubbed by [`redact_all`], so a token in
/// any field never reaches rendered HTML. Redaction runs on the raw text,
/// before HTML escaping, the same way the run and incident views do it.
fn redact_strings(value: &Value) -> Value {
    match value {
        Value::String(s) => Value::String(redact_all(s)),
        Value::Array(items) => Value::Array(items.iter().map(redact_strings).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), redact_strings(item)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) fn format_message_html(target: &str, fields: &Value) -> String {
    let redacted = redact_strings(fields);
    let fields = &redacted;
    let getf = |k: &str| fields.get(k).and_then(Value::as_str).unwrap_or("");
    let getn = |k: &str| -> String {
        fields
            .get(k)
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };

    match target {
        "orbit.policy.deny" => html_pairs(&[
            ("tool", getf("tool").to_string()),
            ("path", getf("path").to_string()),
            ("profile", getf("profile").to_string()),
            ("rule", getf("matched_rule").to_string()),
        ]),
        "orbit.friction.reported" => {
            let mut s = format!(
                "friction reported on {}",
                code_value(getf("task_id").to_string())
            );
            let agent = getf("agent");
            let model = getf("model");
            if !agent.is_empty() || !model.is_empty() {
                s.push_str(" by ");
                s.push_str(&code_value(format!("{agent}/{model}")));
            }
            let summary = getf("summary");
            if !summary.is_empty() {
                s.push_str(": ");
                s.push_str(&escape_html(summary));
            }
            s
        }
        "orbit.job.step_started" => format!(
            "step {} started [run={}]",
            code_value(getf("step_id").to_string()),
            code_value(getf("job_run_id").to_string()),
        ),
        "orbit.job.step_finished" => {
            let step = code_value(getf("step_id").to_string());
            let outcome = code_value(getf("outcome").to_string());
            match fields.get("success").and_then(Value::as_bool) {
                Some(true) => format!("step {step} finished ok ({outcome})"),
                Some(false) | None => format!("step {step} finished {outcome}"),
            }
        }
        "orbit.job.step_retry" => format!(
            "step {} retry attempt={} backoff_ms={}",
            code_value(getf("step_id").to_string()),
            code_value(getn("attempt")),
            code_value(getn("next_backoff_ms")),
        ),
        "orbit.job.step_skipped" => {
            format!(
                "step {} skipped: {}",
                code_value(getf("step_id").to_string()),
                escape_html(getf("reason")),
            )
        }
        "orbit.job.step_denied" => {
            format!(
                "step {} denied: {}",
                code_value(getf("step_id").to_string()),
                escape_html(getf("reason")),
            )
        }
        "orbit.job.fanout" => html_pairs(&[
            ("phase", getf("phase").to_string()),
            ("step", getf("step_id").to_string()),
            ("workers", getn("worker_count")),
            ("collected", getn("collected")),
            ("failed", getn("failed")),
        ]),
        "orbit.job.worker_state" => format!(
            "worker[{}] state={} step={}",
            code_value(getn("worker_index")),
            code_value(getf("state").to_string()),
            code_value(getf("step_id").to_string()),
        ),
        "orbit.job.loop_iteration" => format!(
            "loop {} phase={} step={}",
            code_value(getn("iteration")),
            code_value(getf("phase").to_string()),
            code_value(getf("step_id").to_string()),
        ),
        "orbit.job.loop_did_not_converge" => format!(
            "loop step={} did not converge after {} iterations",
            code_value(getf("step_id").to_string()),
            code_value(getn("max_iterations")),
        ),
        _ if is_agent_relay(target, fields) => format_agent_message(fields),
        _ => format_generic_fields(fields),
    }
}

fn is_agent_relay(target: &str, fields: &Value) -> bool {
    matches!(
        target,
        "orbit_engine::activity_job::cli_runner"
            | "orbit_engine::activity_job::cli_runner::supervisor"
    ) && fields.get("line").and_then(Value::as_str).is_some()
}

fn format_agent_message(fields: &Value) -> String {
    let line = fields.get("line").and_then(Value::as_str).unwrap_or("");
    let event = serde_json::from_str::<Value>(line).ok();
    let kind = event
        .as_ref()
        .and_then(|event| event.get("type"))
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty());
    let item_kind = event
        .as_ref()
        .and_then(|event| event.get("item"))
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty());
    let stream = fields
        .get("stream")
        .and_then(Value::as_str)
        .unwrap_or("output");
    // The kind and abbreviated run fit ahead of the context in the narrow dock
    // and status bar. The complete run remains in the tooltip and fields.
    let mut summary = escape_html(&kind.map_or_else(|| format!("agent {stream}"), str::to_string));
    if let Some(run) = fields.get("job_run_id").and_then(Value::as_str) {
        summary.push_str(" · ");
        summary.push_str(&code_value(run.to_string()));
    }
    // After the run so the first 60 characters still carry kind and run.
    if let Some(item_kind) = item_kind {
        summary.push(' ');
        summary.push_str(&escape_html(item_kind));
    }
    // A structured line is already summarised by its kind; echoing the raw
    // payload would put provider JSON in the status bar. A line without a
    // kind is the only record of what the agent said, so it stays.
    let context = if kind.is_some() {
        let mut fields = fields.clone();
        if let Some(map) = fields.as_object_mut() {
            map.remove("line");
        }
        format_generic_fields(&fields)
    } else {
        format_generic_fields(fields)
    };
    if !context.is_empty() {
        summary.push(' ');
        summary.push_str(&context);
    }
    summary
}

fn format_generic_fields(fields: &Value) -> String {
    let mut parts = Vec::new();
    if let Value::Object(map) = fields {
        if let Some(message) = map.get("message").and_then(Value::as_str) {
            parts.push(escape_html(message));
        }
        // Message first; bulky location and run context last, regardless of
        // the tracing serializer's field order.
        for (key, value) in map
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "message" | "cwd" | "job_run_id"))
            .chain(
                ["cwd", "job_run_id"]
                    .into_iter()
                    .filter_map(|key| map.get_key_value(key)),
            )
        {
            let value = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            parts.push(format!("<b>{}</b>={}", escape_html(key), code_value(value)));
        }
    }
    parts.join(" ")
}

fn html_pairs(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .filter(|(_, value)| !value.is_empty())
        .map(|(key, value)| format!("<b>{}</b>={}", escape_html(key), code_value(value.clone())))
        .collect::<Vec<_>>()
        .join(" ")
}

fn code_value(value: String) -> String {
    let short = shorten_context_value(&value);
    if short != value {
        format!(
            "<code title=\"{}\">{}</code>",
            escape_html(&value),
            escape_html(&short)
        )
    } else {
        format!("<code>{}</code>", escape_html(&value))
    }
}

fn shorten_context_value(value: &str) -> String {
    let home = std::env::var("HOME").ok().filter(|home| !home.is_empty());
    let relative = home
        .as_deref()
        .and_then(|home| Path::new(value).strip_prefix(home).ok());
    // A temporary home can itself live in a managed checkout. Only worktrees
    // below that home override `~`; an outer checkout is not useful context.
    let path = relative.and_then(Path::to_str).unwrap_or(value);
    if Path::new(value).is_absolute()
        && let Some(worktree) = path
            .rsplit_once("/.orbit/state/worktrees/orbit-")
            .map(|(_, worktree)| worktree)
            .or_else(|| path.strip_prefix(".orbit/state/worktrees/orbit-"))
    {
        return worktree.to_string();
    }
    if value.starts_with("jrun-") {
        let parts: Vec<_> = value.split('-').collect();
        if let ["jrun", date, time, suffix] = parts.as_slice()
            && date.len() == 8
            && time.len() == 4
            && date
                .chars()
                .chain(time.chars())
                .all(|ch| ch.is_ascii_digit())
        {
            return format!("jrun-…-{suffix}");
        }
    }
    if let Some(relative) = relative {
        return if relative.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", relative.display())
        };
    }
    value.to_string()
}

fn escape_html(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}
