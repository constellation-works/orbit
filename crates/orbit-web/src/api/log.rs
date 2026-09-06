//! Process-log snapshot and SSE stream handlers.

use std::convert::Infallible;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration as StdDuration;

use axum::body::Body;
use axum::extract::Query;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Json, Response};
use futures_core::Stream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

use super::{LogQuery, map_runtime_error, non_empty_string, server_error};
use crate::log_format::{
    Filters as LogFilters, RenderedLogEvent, parse_matching_event, read_recent_rendered_events,
    render_log_event_for_web, resolve_log_path,
};

const LOG_DEFAULT_LIMIT: usize = 50;
pub(super) const LOG_MAX_LIMIT: usize = 500;
const LOG_STREAM_CHANNEL_DEPTH: usize = 64;
const LOG_STREAM_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
/// Maximum number of concurrent `/api/log/stream` clients. Each accepted
/// stream pins one native polling thread, so this cap bounds thread/FD/CPU
/// usage even if the dashboard is bound beyond loopback.
pub(super) const LOG_STREAM_MAX_CONCURRENT: usize = 8;

/// Connection gate for log SSE streams.
///
/// Wraps a `tokio::sync::Semaphore` so the handler can `try_acquire` a permit
/// per accepted stream. The permit is held by the polling thread and released
/// when the thread exits (which happens within one poll interval after the
/// client disconnects). Excess clients receive `503 Service Unavailable`.
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

    #[cfg(test)]
    pub(super) fn available_permits(&self) -> usize {
        self.sem.available_permits()
    }
}

fn global_log_stream_gate() -> &'static LogStreamGate {
    static GATE: OnceLock<LogStreamGate> = OnceLock::new();
    GATE.get_or_init(|| LogStreamGate::new(LOG_STREAM_MAX_CONCURRENT))
}

/// Set once a shutdown signal is received (see [`crate::shutdown_signal`]), so
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
    match read_log_snapshot_from_path(&path, &q) {
        Ok(events) => Json(events).into_response(),
        Err(e) => map_runtime_error(e),
    }
}

pub(super) async fn stream_log(Query(q): Query<LogQuery>) -> Response {
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
    let stream = ReceiverSseStream {
        rx: spawn_log_sse_frames(path, filters, permit),
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

pub(super) fn log_stream_unavailable() -> Response {
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

// Widened to pub(super) so tests under api/tests/ (per-module layout migration ORB-00224)
// can exercise the snapshot/stream logic without per-handler tests/ subdirs.
pub(super) fn read_log_snapshot_from_path(
    path: &std::path::Path,
    query: &LogQuery,
) -> Result<Vec<RenderedLogEvent>, orbit_core::OrbitError> {
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
    read_recent_rendered_events(path, &filters, limit)
        .map_err(|e| orbit_core::OrbitError::Io(format!("read log {}: {e}", path.display())))
}

// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn log_filters(query: &LogQuery) -> Result<LogFilters, orbit_core::OrbitError> {
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
) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel(LOG_STREAM_CHANNEL_DEPTH);
    thread::spawn(move || {
        // Permit is dropped when this thread exits, which happens within one
        // poll interval of the client disconnecting (tx.is_closed()).
        let _permit = permit;
        let mut offset = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let mut leftover = String::new();
        loop {
            if tx.is_closed() || SHUTTING_DOWN.load(Ordering::Relaxed) {
                return;
            }
            match read_appended_log_events(&path, &filters, &mut offset, &mut leftover) {
                Ok(events) => {
                    for event in events {
                        let frame = match format_sse_frame(&event) {
                            Ok(frame) => frame,
                            Err(_) => continue,
                        };
                        if tx.blocking_send(frame).is_err() {
                            return;
                        }
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {}
            }
            thread::sleep(LOG_STREAM_POLL_INTERVAL);
        }
    });
    rx
}

// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn read_appended_log_events(
    path: &std::path::Path,
    filters: &LogFilters,
    offset: &mut u64,
    leftover: &mut String,
) -> io::Result<Vec<RenderedLogEvent>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < *offset {
        *offset = 0;
        leftover.clear();
    }
    file.seek(SeekFrom::Start(*offset))?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::new();

    loop {
        let mut buf = String::new();
        let n = reader.read_line(&mut buf)?;
        if n == 0 {
            break;
        }
        *offset += n as u64;
        if !buf.ends_with('\n') {
            leftover.push_str(&buf);
            continue;
        }
        let mut full_line = String::new();
        if !leftover.is_empty() {
            full_line.push_str(leftover);
            leftover.clear();
        }
        full_line.push_str(buf.trim_end_matches('\n'));
        if let Some(event) = parse_matching_event(&full_line, filters) {
            events.push(render_log_event_for_web(&event));
        }
    }

    Ok(events)
}

// Widened to pub(super) for api/tests/ access after test layout migration (ORB-00224).
pub(super) fn format_sse_frame(event: &RenderedLogEvent) -> Result<String, serde_json::Error> {
    serde_json::to_string(event).map(|json| format!("data: {json}\n\n"))
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
