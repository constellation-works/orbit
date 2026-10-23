//! Process-log snapshot and SSE stream handlers.

use std::convert::Infallible;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration as StdDuration;

use axum::body::Body;
use axum::extract::Query;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use futures_core::Stream;
use serde::Serialize;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use super::{LogQuery, map_runtime_error, non_empty_string, server_error};
use crate::log_format::{
    Filters as LogFilters, RenderedLogEvent, log_file_generation, parse_matching_event,
    read_recent_rendered_tail, render_log_event_for_web, resolve_log_path,
};

const LOG_DEFAULT_LIMIT: usize = 50;
const LOG_MAX_LIMIT: usize = 500;

/// Snapshot body for `GET /api/log`. `cursor` binds complete-record offsets
/// to each scanned file's generation, so `/api/log/stream?cursor=` resumes
/// without skipping a replacement file. Numeric offsets remain for older clients.
#[derive(Debug, Serialize)]
struct LogSnapshot {
    events: Vec<RenderedLogEvent>,
    offset: u64,
    agent_offset: u64,
    cursor: String,
}

const LOG_STREAM_CHANNEL_DEPTH: usize = 64;
/// Poll interval while log data is flowing; also the floor after a reset.
const LOG_STREAM_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
/// Ceiling of the idle backoff. An idle stream polls this often, so a new
/// line reaches the client at most this late and a disconnect or shutdown is
/// noticed within it.
const LOG_STREAM_IDLE_POLL_INTERVAL: StdDuration = StdDuration::from_secs(1);

/// Idle backoff for the stream's polling thread: fast while the log advances,
/// doubling toward [`LOG_STREAM_IDLE_POLL_INTERVAL`] while nothing new is
/// read, and back to the floor as soon as data arrives.
#[derive(Debug)]
struct PollBackoff {
    delay: StdDuration,
}

impl PollBackoff {
    fn new() -> Self {
        Self {
            delay: LOG_STREAM_POLL_INTERVAL,
        }
    }

    /// Delay before the next poll, given whether the last poll made progress.
    fn next_delay(&mut self, progressed: bool) -> StdDuration {
        if progressed {
            self.delay = LOG_STREAM_POLL_INTERVAL;
        } else {
            self.delay = (self.delay * 2).min(LOG_STREAM_IDLE_POLL_INTERVAL);
        }
        self.delay
    }
}
/// Maximum number of concurrent `/api/log/stream` clients. Each accepted
/// stream pins one native polling thread, so this cap bounds thread/FD/CPU
/// usage even if the dashboard is bound beyond loopback.
const LOG_STREAM_MAX_CONCURRENT: usize = 8;

/// Connection gate for log SSE streams.
///
/// Wraps a `tokio::sync::Semaphore` so the handler can `try_acquire` a permit
/// per accepted stream. The permit is held by the polling thread and released
/// when the thread exits (which happens within one idle poll interval after
/// the client disconnects). Excess clients receive `503 Service Unavailable`.
pub(super) struct LogStreamGate {
    sem: Arc<Semaphore>,
}

impl LogStreamGate {
    pub(super) fn new(max: usize) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(max)),
        }
    }

    pub(super) fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.sem).try_acquire_owned().ok()
    }
}

fn global_log_stream_gate() -> &'static LogStreamGate {
    static GATE: OnceLock<LogStreamGate> = OnceLock::new();
    GATE.get_or_init(|| LogStreamGate::new(LOG_STREAM_MAX_CONCURRENT))
}

/// Set once a shutdown signal is received (see [`crate::serve::shutdown_signal`]), so
/// every open (and future) `/api/log/stream` polling thread closes on its next
/// tick instead of running until the client disconnects. An open stream that
/// outlives the client is exactly the ownership gap that let a live restart
/// hang past `orbit-web.service`'s `TimeoutStopUSec` until systemd's SIGKILL
/// (ORB-11246): the stream is this handler's resource, so it must close
/// itself, not wait to be told by a client that may not respond in time.
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);

/// Tell every open (and future) log-stream polling thread to close now.
pub(super) fn request_shutdown() {
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

pub(super) async fn get_log(Query(q): Query<LogQuery>) -> Response {
    let path = match resolve_log_path(None) {
        Ok(path) => path,
        Err(e) => return map_runtime_error(e),
    };
    // File scan is blocking IO; run it on the pool, not the request worker.
    match super::blocking("log snapshot", move || {
        read_log_snapshot_from_path(&path, &q)
    })
    .await
    {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(response) => *response,
    }
}

pub(super) async fn stream_log(Query(q): Query<LogQuery>, headers: HeaderMap) -> Response {
    let permit = match global_log_stream_gate().try_acquire() {
        Some(p) => p,
        None => return log_stream_unavailable(),
    };
    let path = match resolve_log_path(None) {
        Ok(path) => path,
        Err(e) => return map_runtime_error(e),
    };
    let filters = match log_filters(&q) {
        Ok(filters) => filters,
        Err(e) => return map_runtime_error(e),
    };
    let cursor = last_event_id_header(&headers)
        .and_then(parse_stream_cursor)
        .or_else(|| q.cursor.as_deref().and_then(parse_stream_cursor));
    let (resume, agent_resume) = match cursor {
        Some((operational, agent)) => (
            Some(operational),
            agent.or_else(|| q.agent_from.map(FeedCursor::legacy)),
        ),
        None => (
            q.from.map(FeedCursor::legacy),
            q.agent_from.map(FeedCursor::legacy),
        ),
    };
    let stream = ReceiverSseStream {
        rx: spawn_log_sse_frames(path, filters, permit, resume, agent_resume),
    };
    match Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .header(header::CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
    {
        Ok(response) => response,
        Err(e) => server_error(orbit_core::OrbitError::Execution(format!(
            "build SSE response: {e}"
        ))),
    }
}

fn log_stream_unavailable() -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": format!(
                "log stream concurrency limit reached (max {LOG_STREAM_MAX_CONCURRENT}); retry shortly"
            )
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    response
}

fn read_log_snapshot_from_path(
    path: &std::path::Path,
    query: &LogQuery,
) -> Result<LogSnapshot, orbit_core::OrbitError> {
    let limit = match query.limit {
        Some(limit) if limit > LOG_MAX_LIMIT => {
            return Err(orbit_core::OrbitError::InvalidInput(format!(
                "limit must be <= {LOG_MAX_LIMIT}; got {limit}"
            )));
        }
        Some(limit) => limit,
        None => LOG_DEFAULT_LIMIT,
    };
    let filters = log_filters(query)?;
    let mut tail = read_recent_rendered_tail(path, &filters, limit)
        .map_err(|e| orbit_core::OrbitError::Io(format!("read log {}: {e}", path.display())))?;
    let mut agent_offset = 0;
    let mut agent_cursor = None;
    if let Some(agent_path) = orbit_common::observability::logging::agent_jsonl_log_path(path) {
        let agent_tail = read_recent_rendered_tail(&agent_path, &filters, limit).map_err(|e| {
            orbit_core::OrbitError::Io(format!("read log {}: {e}", agent_path.display()))
        })?;
        agent_offset = agent_tail.cursor;
        agent_cursor = Some(FeedCursor {
            offset: agent_tail.cursor,
            generation: Some(agent_tail.generation),
        });
        tail.events.extend(agent_tail.events);
        tail.events.sort_by(|left, right| left.ts.cmp(&right.ts));
        let discard = tail.events.len().saturating_sub(limit);
        tail.events.drain(..discard);
    }
    // The cursor comes from the scanned extent itself, never a later `stat`:
    // a line appended after the scan lies beyond it and reaches the client
    // through `?cursor=` / `Last-Event-ID` instead of being skipped.
    let cursor = format_stream_cursor(
        &FeedCursor {
            offset: tail.cursor,
            generation: Some(tail.generation),
        },
        agent_cursor.as_ref(),
    );
    Ok(LogSnapshot {
        cursor,
        events: tail.events,
        offset: tail.cursor,
        agent_offset,
    })
}

/// A byte position and, for rotation-safe clients, the file it belongs to.
#[derive(Clone, Debug)]
struct FeedCursor {
    offset: u64,
    generation: Option<String>,
}

impl FeedCursor {
    fn legacy(offset: u64) -> Self {
        Self {
            offset,
            generation: None,
        }
    }

    fn at_end(path: &std::path::Path) -> Self {
        // Capture identity and extent together, including before the first poll.
        std::fs::metadata(path)
            .ok()
            .and_then(|metadata| {
                Some(Self {
                    offset: metadata.len(),
                    generation: Some(log_file_generation(&metadata).ok()?),
                })
            })
            .unwrap_or(Self {
                offset: 0,
                generation: Some("missing".into()),
            })
    }

    fn encode(&self) -> String {
        match &self.generation {
            Some(generation) => format!("{}@{generation}", self.offset),
            None => self.offset.to_string(),
        }
    }
}

/// IDs are opaque to consumers. Accept legacy byte-only IDs deliberately,
/// but never pair an invalid generation with a trusted offset.
fn parse_stream_cursor(raw: &str) -> Option<(FeedCursor, Option<FeedCursor>)> {
    fn feed(raw: &str) -> Option<FeedCursor> {
        let (offset, generation) = match raw.split_once('@') {
            Some((offset, generation)) => {
                if generation.is_empty()
                    || generation.len() > 128
                    || !generation
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                {
                    return None;
                }
                (offset, Some(generation.to_owned()))
            }
            None => (raw, None),
        };
        Some(FeedCursor {
            offset: offset.parse().ok()?,
            generation,
        })
    }
    let mut feeds = raw.trim().split(':');
    let operational = feed(feeds.next()?)?;
    let agent = match feeds.next() {
        Some(raw) => Some(feed(raw)?),
        None => None,
    };
    if feeds.next().is_some() {
        return None;
    }
    Some((operational, agent))
}

fn format_stream_cursor(operational: &FeedCursor, agent: Option<&FeedCursor>) -> String {
    match agent {
        Some(agent) => format!("{}:{}", operational.encode(), agent.encode()),
        None => operational.encode(),
    }
}

fn last_event_id_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
}

fn log_filters(query: &LogQuery) -> Result<LogFilters, orbit_core::OrbitError> {
    LogFilters::from_query_parts(
        query.target.as_deref().and_then(non_empty_string),
        query.level.as_deref().and_then(non_empty_string),
        query
            .since
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
}

fn spawn_log_sse_frames(
    path: PathBuf,
    filters: LogFilters,
    permit: OwnedSemaphorePermit,
    resume_offset: Option<FeedCursor>,
    agent_resume_offset: Option<FeedCursor>,
) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(LOG_STREAM_CHANNEL_DEPTH);
    let start = resume_offset.unwrap_or_else(|| FeedCursor::at_end(&path));
    let agent_path = orbit_common::observability::logging::agent_jsonl_log_path(&path);
    let agent_start = agent_resume_offset.unwrap_or_else(|| {
        agent_path
            .as_ref()
            .map_or_else(|| FeedCursor::legacy(0), |path| FeedCursor::at_end(path))
    });
    thread::spawn(move || {
        // Permit is dropped when this thread exits, which happens within one
        // idle poll interval — or one batch of a replay — of the client
        // disconnecting (tx.is_closed()).
        let _permit = permit;
        let mut offset = start.offset;
        let mut agent_offset = agent_start.offset;
        let mut complete = start.clone();
        let mut complete_agent = agent_start.clone();
        let mut lines = LogLineBuffer {
            identity: start.generation,
            ..LogLineBuffer::default()
        };
        let mut agent_lines = LogLineBuffer {
            identity: agent_start.generation,
            ..LogLineBuffer::default()
        };
        let mut backoff = PollBackoff::new();
        loop {
            if tx.is_closed() || SHUTTING_DOWN.load(Ordering::Relaxed) {
                return;
            }
            // A replay (`from=0` over a large log) drains batch by batch with
            // no sleep between them, re-checking disconnect and shutdown each
            // time; only a caught-up stream waits for the next poll.
            let scanned_from = offset;
            let mut progressed = false;
            let mut caught_up =
                match read_appended_log_events(&path, &filters, &mut offset, &mut lines) {
                    Ok(batch) => {
                        complete = FeedCursor {
                            offset: batch.cursor,
                            generation: lines.identity.clone(),
                        };
                        for (event, event_offset) in batch.events {
                            let frame = match format_feed_sse_frame(
                                &event,
                                &FeedCursor {
                                    offset: event_offset,
                                    generation: lines.identity.clone(),
                                },
                                agent_path.as_ref().map(|_| &complete_agent),
                            ) {
                                Ok(frame) => frame,
                                Err(_) => continue,
                            };
                            if tx.blocking_send(frame).is_err() {
                                return;
                            }
                        }
                        !batch.more
                    }
                    Err(_) => true,
                };
            progressed |= offset != scanned_from;
            if let Some(agent_path) = &agent_path {
                let scanned_from = agent_offset;
                if let Ok(batch) = read_appended_log_events(
                    agent_path,
                    &filters,
                    &mut agent_offset,
                    &mut agent_lines,
                ) {
                    caught_up &= !batch.more;
                    complete_agent = FeedCursor {
                        offset: batch.cursor,
                        generation: agent_lines.identity.clone(),
                    };
                    for (event, event_offset) in batch.events {
                        let frame = match format_feed_sse_frame(
                            &event,
                            &complete,
                            Some(&FeedCursor {
                                offset: event_offset,
                                generation: agent_lines.identity.clone(),
                            }),
                        ) {
                            Ok(frame) => frame,
                            Err(_) => continue,
                        };
                        if tx.blocking_send(frame).is_err() {
                            return;
                        }
                    }
                }
                progressed |= agent_offset != scanned_from;
            }
            // Progress means bytes were consumed (even if the filter matched
            // none of them), so a busy log with a narrow filter stays fast.
            let delay = backoff.next_delay(progressed);
            if caught_up {
                thread::sleep(delay);
            }
        }
    });
    rx
}

/// Most matching events one [`read_appended_log_events`] call renders.
const LOG_STREAM_BATCH_EVENTS: usize = 256;
/// Most bytes one [`read_appended_log_events`] call scans, so a filter that
/// matches nothing still yields to the disconnect/shutdown check regularly.
const LOG_STREAM_BATCH_BYTES: u64 = 1 << 20;
/// Longest record the stream reassembles. A longer one — or an unterminated
/// run of bytes that never gets its newline — is dropped through its next
/// newline, so partial-line storage never exceeds this.
pub(super) const LOG_STREAM_MAX_RECORD_BYTES: usize = crate::log_format::MAX_LOG_RECORD_BYTES;

/// Bytes of the record being read, carried across polls until its newline.
#[derive(Debug, Default)]
pub(super) struct LogLineBuffer {
    pub(super) partial: Vec<u8>,
    /// Set once the current record exceeded [`LOG_STREAM_MAX_RECORD_BYTES`];
    /// its remaining bytes are skipped up to and including the next newline.
    pub(super) discarding: bool,
    /// Cursor after the last complete record. Companion-feed event IDs must
    /// not advance past an unfinished record that a reconnect cannot rebuild.
    complete_offset: u64,
    identity: Option<String>,
}

impl LogLineBuffer {
    fn clear(&mut self) {
        self.partial.clear();
        self.discarding = false;
        self.complete_offset = 0;
    }

    fn push(&mut self, bytes: &[u8]) {
        if self.discarding {
            return;
        }
        if self.partial.len() + bytes.len() > LOG_STREAM_MAX_RECORD_BYTES {
            // Release the allocation, not just the length.
            self.partial = Vec::new();
            self.discarding = true;
            return;
        }
        self.partial.extend_from_slice(bytes);
    }

    /// End the current record at its newline; `None` when it was discarded.
    fn finish(&mut self) -> Option<String> {
        if std::mem::take(&mut self.discarding) {
            tracing::warn!(
                limit = LOG_STREAM_MAX_RECORD_BYTES,
                "log stream skipped an oversized record"
            );
            return None;
        }
        let line = String::from_utf8_lossy(&self.partial).into_owned();
        self.partial.clear();
        Some(line)
    }
}

/// One bounded read of the log: the rendered matches, and whether unread
/// bytes remain past `offset` so the caller should read again without waiting.
#[derive(Debug)]
pub(super) struct AppendedLogBatch {
    pub(super) events: Vec<(RenderedLogEvent, u64)>,
    pub(super) more: bool,
    pub(super) cursor: u64,
}

/// Read complete lines appended since `offset`, keeping a trailing partial
/// line in `lines` until its newline arrives.
///
/// One call stops after [`LOG_STREAM_BATCH_EVENTS`] matches or
/// [`LOG_STREAM_BATCH_BYTES`] scanned bytes, whichever comes first, leaving
/// `offset` at the resume point.
///
/// Lines are read as bytes and decoded lossily: a torn write or a resume
/// offset inside a multi-byte character must cost one malformed line, not
/// fail every later poll at the same offset and stall the stream.
// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn read_appended_log_events(
    path: &std::path::Path,
    filters: &LogFilters,
    offset: &mut u64,
    lines: &mut LogLineBuffer,
) -> io::Result<AppendedLogBatch> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    let len = metadata.len();
    let identity = log_file_generation(&metadata)?;
    if lines
        .identity
        .as_ref()
        .is_some_and(|prior| *prior != identity)
    {
        *offset = 0;
        lines.clear();
    }
    lines.identity = Some(identity);
    if len < *offset {
        *offset = 0;
        lines.clear();
    }
    if lines.partial.is_empty() && !lines.discarding {
        lines.complete_offset = *offset;
    }
    file.seek(SeekFrom::Start(*offset))?;
    let budget = (len - *offset).min(LOG_STREAM_BATCH_BYTES);
    let mut reader = BufReader::new(file.take(budget));
    let mut events = Vec::new();

    while events.len() < LOG_STREAM_BATCH_EVENTS {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let newline = buf.iter().position(|&b| b == b'\n');
        let chunk = newline.map_or(buf, |at| &buf[..at]);
        lines.push(chunk);
        let consumed = chunk.len() + usize::from(newline.is_some());
        reader.consume(consumed);
        *offset += consumed as u64;
        if newline.is_none() {
            continue;
        }
        lines.complete_offset = *offset;
        if let Some(event) = lines
            .finish()
            .and_then(|line| parse_matching_event(&line, filters))
        {
            events.push((render_log_event_for_web(&event), *offset));
        }
    }

    Ok(AppendedLogBatch {
        events,
        more: *offset < len,
        cursor: lines.complete_offset,
    })
}

fn format_feed_sse_frame(
    event: &RenderedLogEvent,
    operational: &FeedCursor,
    agent: Option<&FeedCursor>,
) -> Result<String, serde_json::Error> {
    let cursor = format_stream_cursor(operational, agent);
    serde_json::to_string(event).map(|json| format!("id: {cursor}\ndata: {json}\n\n"))
}

struct ReceiverSseStream {
    rx: mpsc::Receiver<String>,
}

impl Stream for ReceiverSseStream {
    type Item = Result<String, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx).map(|item| item.map(Ok))
    }
}
