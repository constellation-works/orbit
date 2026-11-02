//! A stdio MCP session that survives replacing the server's executable.
//!
//! `orbit mcp serve` outlives upgrades. Between calls it asks whether the
//! installed executable still names its running image; when a newer one
//! replaced it — or a breaking upgrade is waiting for this process to yield —
//! it hands the session over at an idle point instead of holding the
//! generation or dropping its client.
//!
//! Handing over needs exact control of stdin: bytes a reader has buffered but
//! not delivered would be lost across `exec`. So this module reads fd 0
//! itself, without buffering beyond one partial line, and passes complete
//! lines to the MCP service. It tracks which requests are outstanding from
//! the lines it forwards and the responses the service flushes. The session
//! is idle when every forwarded line has been consumed and every request the
//! transport accepts has been answered. Rejected or dropped input never adds
//! a pending request, since its error may not echo the original id. The pump
//! writes that id-less invalid-request error itself, as one complete frame on
//! the same stdout writer the service uses: rmcp sends it from inside
//! `receive` after discarding the line, and the service loop drops that send
//! when another branch is ready.
//!
//! At that point it stops reading, stops the service, and returns
//! [`StdioExit::HandOver`] with the client's original `initialize` request
//! and any partial line it had read. A request can arrive between the idle
//! check and the reader stopping, so stopping the reader keeps the service's
//! input open: the service answers that request and waits to be stopped,
//! rather than reading end of input as the client leaving. The reader defers
//! a handover while that partial line exceeds [`MAX_CARRYOVER`], keeping the
//! service alive until a later idle check. The caller execs the new
//! executable with that state in [`RESUME_ENV`]. Because `exec` keeps the pid
//! and the stdio descriptors, the client sees one uninterrupted session. The
//! new process replays the `initialize` request into its server rather than
//! expecting a second handshake. Replay keeps the client's metadata and
//! restores the protocol version that handshake agreed.

use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine as _;
use bytes::BytesMut;
use orbit_common::OrbitError;
use orbit_common::fs::generation;
use rmcp::ServiceExt;
use rmcp::model::{
    ClientJsonRpcMessage, ClientNotification, ClientRequest, ErrorData, InitializeRequestParams,
    ServerJsonRpcMessage,
};
use rmcp::transport::async_rw::{JsonRpcMessageCodec, JsonRpcMessageCodecError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;
use tokio_util::codec::Decoder as _;

use crate::adapter::OrbitToolServer;

/// Carries a resumed session's state into the replacement image.
pub const RESUME_ENV: &str = "ORBIT_MCP_RESUME";

/// How often an idle session checks for a replacement or a pending switch.
const LIFECYCLE_INTERVAL: Duration = Duration::from_secs(2);

/// Debug-build fixture only. Milliseconds between stdio lifecycle checks.
/// Release builds keep the two-second budget whatever this says. Values
/// outside 20ms..=2s are ignored so a fixture can only shorten the tick.
pub const LIFECYCLE_INTERVAL_ENV: &str = "ORBIT_TEST_MCP_LIFECYCLE_INTERVAL_MS";

fn lifecycle_interval_from_env(value: Option<&str>, debug_build: bool) -> Duration {
    if debug_build
        && let Some(milliseconds) = value.and_then(|raw| raw.parse::<u64>().ok())
        && (20..=2_000).contains(&milliseconds)
    {
        return Duration::from_millis(milliseconds);
    }
    LIFECYCLE_INTERVAL
}

fn lifecycle_interval() -> Duration {
    lifecycle_interval_from_env(
        std::env::var(LIFECYCLE_INTERVAL_ENV).ok().as_deref(),
        cfg!(debug_assertions),
    )
}
/// A partial line longer than this is not handed over; the session keeps
/// serving and retries at a later idle point.
const MAX_CARRYOVER: usize = 32 * 1024;
const READ_CHUNK: usize = 64 * 1024;
/// Match rmcp's stream limit, including the terminating newline.
const MAX_STDIO_LINE: usize = rmcp::transport::async_rw::DEFAULT_MAX_LINE_LENGTH;

/// How a stdio session ended.
pub enum StdioExit {
    /// The client closed the session.
    Closed,
    /// A pending breaking upgrade asked this process to yield, and no
    /// installed executable can resume the session.
    Yielded,
    /// Replace this process with `executable`, exporting `resume` as
    /// [`RESUME_ENV`].
    HandOver { executable: PathBuf, resume: String },
}

/// The session state one image hands the next.
#[derive(Serialize, Deserialize)]
struct ResumeState {
    /// The process that handed over. `exec` keeps the pid, so a child that
    /// merely inherited the environment can never mistake itself for the
    /// resumed server.
    pid: u32,
    /// The client's `initialize` params as sent, if it initialized.
    initialize: Option<Value>,
    /// Bytes of a partial line read before the handover, base64-encoded.
    carryover: String,
}

/// A session this process was exec'd to resume.
pub(crate) struct Resumed {
    initialize: Option<InitializeRequestParams>,
    carryover: Vec<u8>,
}

/// Read the handed-over session, when this image was exec'd to resume one.
pub(crate) fn resumed_session() -> Option<Resumed> {
    let raw = std::env::var(RESUME_ENV).ok()?;
    let state = match serde_json::from_str::<ResumeState>(&raw) {
        Ok(state) if state.pid == std::process::id() => state,
        Ok(_) => return None,
        Err(error) => {
            tracing::warn!(target: "orbit.mcp", "ignoring an unreadable resumed session: {error}");
            return None;
        }
    };
    let carryover = base64::engine::general_purpose::STANDARD
        .decode(state.carryover)
        .ok()?;
    let initialize = match state.initialize.map(serde_json::from_value) {
        Some(Ok(request)) => Some(request),
        Some(Err(error)) => {
            tracing::warn!(target: "orbit.mcp", "ignoring an unreadable resumed initialize: {error}");
            return None;
        }
        None => None,
    };
    Some(Resumed {
        initialize,
        carryover,
    })
}

/// What an idle session should do next.
enum Lifecycle {
    Continue,
    Yield,
    HandOver(PathBuf),
}

fn lifecycle() -> Lifecycle {
    if generation::process_participation().is_none() {
        return Lifecycle::Continue;
    }
    if let Some(executable) = generation::handover_target(Some(generation::RESUME_MCP_STDIO)) {
        return Lifecycle::HandOver(executable);
    }
    if generation::pending_switch_for_this_process().is_some() {
        return Lifecycle::Yield;
    }
    Lifecycle::Continue
}

/// Requests forwarded to the service and not yet answered, keyed by their
/// JSON-RPC id's JSON text.
#[derive(Default)]
struct Tracker {
    pending: HashSet<String>,
    /// The client's `initialize` params, as sent.
    initialize: Option<Value>,
}

impl Tracker {
    fn forwarded(&mut self, line: &[u8]) {
        // Match rmcp's incoming message type, including its BOM tolerance.
        // A raw method/id pair is insufficient. Lines that do not decode are
        // not pending: the pump answers an invalid request without forwarding
        // it, and the transport drops broken JSON and compatibility-path
        // notifications without a response.
        let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        let Ok(message) = serde_json::from_slice::<ClientJsonRpcMessage>(line) else {
            return;
        };
        match message {
            ClientJsonRpcMessage::Request(request) => {
                let Ok(id) = serde_json::to_string(&request.id) else {
                    return;
                };
                if matches!(request.request, ClientRequest::InitializeRequest(_)) {
                    self.initialize = serde_json::from_slice::<Value>(line)
                        .ok()
                        .and_then(|message| message.get("params").cloned());
                }
                self.pending.insert(id);
            }
            ClientJsonRpcMessage::Notification(notification) => {
                if let ClientNotification::CancelledNotification(cancelled) =
                    notification.notification
                    && let Some(id) = cancelled.params.request_id
                    && let Ok(id) = serde_json::to_string(&id)
                {
                    self.pending.remove(&id);
                }
            }
            _ => {}
        }
    }

    fn answered(&mut self, line: &[u8]) {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        for message in messages(message) {
            if message.get("method").is_none()
                && (message.get("result").is_some() || message.get("error").is_some())
                && let Some(id) = message.get("id")
            {
                self.pending.remove(&id.to_string());
            }
        }
    }
}

fn messages(message: Value) -> Vec<Value> {
    match message {
        Value::Array(batch) => batch,
        single => vec![single],
    }
}

/// A line rmcp's `AsyncRwTransport::receive` would answer with an id-less
/// `-32600` after consuming it. Broken JSON and the non-standard
/// notification compatibility path are excluded: rmcp writes nothing for
/// those, and answering them would steal the next read.
///
/// Classification goes through rmcp's public line codec, which already
/// decides the compatibility path. A copied method list would drift.
fn rmcp_receive_would_reject(line: &[u8]) -> bool {
    let mut codec = JsonRpcMessageCodec::<ClientJsonRpcMessage>::new();
    let mut buffer = BytesMut::from(line);
    match codec.decode(&mut buffer) {
        Err(JsonRpcMessageCodecError::Serde(error)) => matches!(
            error.classify(),
            serde_json::error::Category::Data | serde_json::error::Category::Io
        ),
        _ => false,
    }
}

/// Same bytes `ErrorData::invalid_request("Invalid request", None)`
/// serializes to. The write shares [`write_complete_stdout_frame`] with
/// service responses and runs to completion on this task, outside the
/// service `select!` that cancels `receive`.
async fn write_stdio_invalid_request() -> bool {
    let message =
        ServerJsonRpcMessage::error(ErrorData::invalid_request("Invalid request", None), None);
    let Ok(mut line) = serde_json::to_vec(&message) else {
        return false;
    };
    line.push(b'\n');
    tracing::debug!(
        target: "orbit.mcp",
        "answering an undecodable stdio line before the transport reads it"
    );
    tokio::task::spawn_blocking(move || write_complete_stdout_frame(&line).is_ok())
        .await
        .unwrap_or(false)
}

/// Serve `server` over stdio until the client leaves or the session is
/// handed over. `resumed` replays a session handed over by a previous image.
pub(crate) async fn serve(
    server: OrbitToolServer,
    resumed: Option<Resumed>,
) -> Result<StdioExit, OrbitError> {
    #[cfg(unix)]
    if let Some(stdin) = unix::NonBlockingStdin::new() {
        return serve_handing_over(server, resumed, stdin).await;
    }
    serve_plain(server, resumed).await
}

/// Whether a stdio session in this process could hand over: `serve` does
/// only when stdin can be polled. A process registers this before it serves,
/// so an updater knows which sessions follow a renamed candidate.
pub fn stdin_supports_handover() -> bool {
    #[cfg(unix)]
    {
        unix::stdin_is_pollable()
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// Without a pollable stdin (a regular file, `/dev/null`) the session is
/// never handed over. It still reads stdin line by line itself, so a line
/// rmcp would reject is answered before rmcp can consume and drop it.
async fn serve_plain(
    server: OrbitToolServer,
    resumed: Option<Resumed>,
) -> Result<StdioExit, OrbitError> {
    use tokio::io::AsyncReadExt as _;
    let (carryover, initialize) = match resumed {
        Some(resumed) => (resumed.carryover, resumed.initialize),
        None => (Vec::new(), None),
    };
    let undelivered = Arc::new(AtomicUsize::new(0));
    let (lines, receiver) = mpsc::channel::<Vec<u8>>(64);
    let stdin = std::io::Cursor::new(carryover).chain(tokio::io::stdin());
    let pump = tokio::spawn(pump_plain_lines(stdin, lines, Arc::clone(&undelivered)));
    let reader = LineReader {
        lines: receiver,
        current: Vec::new(),
        position: 0,
        undelivered,
    };
    let transport = (reader, CompleteFrameStdout::new());
    let running = match initialize {
        Some(request) => {
            let peer_info = match server.resumed_peer_info(request) {
                Ok(peer_info) => peer_info,
                Err(error) => {
                    pump.abort();
                    let _ = pump.await;
                    return Err(start_error(&error));
                }
            };
            rmcp::service::serve_directly(server, transport, Some(peer_info))
        }
        None => match server.serve(transport).await {
            Ok(running) => running,
            Err(error) => {
                pump.abort();
                let _ = pump.await;
                return Err(start_error(&error));
            }
        },
    };
    let result = running
        .waiting()
        .await
        .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")));
    pump.abort();
    let _ = pump.await;
    result?;
    Ok(StdioExit::Closed)
}

/// Read complete lines from a non-pollable stdin and forward them to the
/// service, answering a rejected line here instead of forwarding it. This
/// mirrors the unix pump's `forward_lines`: rmcp's `receive` clears such a
/// line and then sends its error from a `select!` branch that a ready
/// response task cancels, so the client would never see the rejection.
async fn pump_plain_lines<R>(
    mut reader: R,
    lines: mpsc::Sender<Vec<u8>>,
    undelivered: Arc<AtomicUsize>,
) where
    R: AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncReadExt as _;
    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK];
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) => {
                if !buffer.is_empty() {
                    let line = std::mem::take(&mut buffer);
                    undelivered.fetch_add(1, Ordering::SeqCst);
                    let _ = lines.send(line).await;
                }
                return;
            }
            Ok(read) => read,
            Err(_) => return,
        };
        let mut remaining = &chunk[..read];
        while !remaining.is_empty() {
            let end = remaining.iter().position(|byte| *byte == b'\n');
            let take = end.map_or(remaining.len(), |index| index + 1);
            if buffer.len() + take > MAX_STDIO_LINE {
                tracing::warn!(target: "orbit.mcp", "closing stdio session: input line exceeds the transport limit");
                return;
            }
            buffer.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
            if end.is_some() && !forward_plain_lines(&mut buffer, &lines, &undelivered).await {
                return;
            }
        }
    }
}

/// Forward every complete line in `buffer`, leaving a partial one. False
/// when the service stopped listening or the rejection could not be written.
async fn forward_plain_lines(
    buffer: &mut Vec<u8>,
    lines: &mpsc::Sender<Vec<u8>>,
    undelivered: &AtomicUsize,
) -> bool {
    while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=end).collect();
        if rmcp_receive_would_reject(&line) {
            if !write_stdio_invalid_request().await {
                return false;
            }
            continue;
        }
        undelivered.fetch_add(1, Ordering::SeqCst);
        if lines.send(line).await.is_err() {
            return false;
        }
    }
    true
}

fn start_error(error: &dyn std::fmt::Display) -> OrbitError {
    OrbitError::Execution(format!("mcp serve_stdio start: {error}"))
}

#[cfg(unix)]
async fn serve_handing_over(
    server: OrbitToolServer,
    resumed: Option<Resumed>,
    stdin: unix::NonBlockingStdin,
) -> Result<StdioExit, OrbitError> {
    let tracker = Arc::new(Mutex::new(Tracker::default()));
    let undelivered = Arc::new(AtomicUsize::new(0));
    let (lines, receiver) = mpsc::channel::<Vec<u8>>(64);
    let (stop, stopped) = mpsc::channel(1);
    let (carryover, initialize) = match resumed {
        Some(resumed) => (resumed.carryover, resumed.initialize),
        None => (Vec::new(), None),
    };
    if let Some(request) = &initialize {
        let mut tracker = lock(&tracker);
        tracker.initialize = serde_json::to_value(request).ok();
    }
    let pump = tokio::spawn(unix::pump(
        stdin,
        carryover,
        lines,
        Arc::clone(&tracker),
        Arc::clone(&undelivered),
        stopped,
    ));
    let reader = LineReader {
        lines: receiver,
        current: Vec::new(),
        position: 0,
        undelivered: Arc::clone(&undelivered),
    };
    let writer = TrackingWriter {
        inner: CompleteFrameStdout::new(),
        line: Vec::new(),
        answered: Vec::new(),
        tracker: Arc::clone(&tracker),
    };
    let running = match initialize {
        Some(request) => {
            let peer_info = server
                .resumed_peer_info(request)
                .map_err(|error| start_error(&error))?;
            tracing::info!(target: "orbit.mcp", "resumed the stdio session after an executable handover");
            rmcp::service::serve_directly(server, (reader, writer), Some(peer_info))
        }
        None => server
            .serve((reader, writer))
            .await
            .map_err(|error| start_error(&error))?,
    };
    let cancel = running.cancellation_token();
    let mut service = tokio::spawn(running.waiting());
    let mut interval = tokio::time::interval(lifecycle_interval());
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            finished = &mut service => {
                finished
                    .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))?
                    .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))?;
                let _ = stop.send(unix::PumpStop::Always).await;
                let _ = pump.await;
                return Ok(StdioExit::Closed);
            }
            _ = interval.tick() => {}
        }
        if !is_idle(&tracker, &undelivered) {
            continue;
        }
        let decision = tokio::task::spawn_blocking(lifecycle)
            .await
            .unwrap_or(Lifecycle::Continue);
        if matches!(decision, Lifecycle::Continue) || !is_idle(&tracker, &undelivered) {
            continue;
        }
        // Let the pump check its own buffer before stopping: checking a
        // shared length here would race with another stdin read. A deferred
        // handover leaves both the reader and the service running.
        if matches!(decision, Lifecycle::HandOver(_)) {
            let (accepted, response) = tokio::sync::oneshot::channel();
            let _ = stop.send(unix::PumpStop::HandOver(accepted)).await;
            if matches!(response.await, Ok(false)) {
                tracing::debug!(
                    target: "orbit.mcp.handover",
                    handover_deferred = true,
                    "deferring executable handover until the partial request completes"
                );
                continue;
            }
        } else {
            let _ = stop.send(unix::PumpStop::Always).await;
        }
        // Wait out whatever was forwarded before the pump stopped: a
        // request answered, a notification consumed.
        let Ok(Ok(outcome)) = pump.await else {
            return Err(OrbitError::Execution(
                "mcp serve_stdio: the stdin reader stopped unexpectedly".into(),
            ));
        };
        // Hold the input open until the service is cancelled: on EOF the
        // service answers what it was forwarded and finishes, which would end
        // the session below instead of handing it over.
        let (carryover, _input) = match outcome {
            unix::PumpOutcome::Stopped { carryover, input } => (carryover, input),
            unix::PumpOutcome::Closed => {
                // The client left while this session was deciding.
                let _ = service.await;
                return Ok(StdioExit::Closed);
            }
        };
        while !is_idle(&tracker, &undelivered) {
            tokio::select! {
                _ = &mut service => return Ok(StdioExit::Closed),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }
        // Let the service finish handling a consumed notification.
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        let _ = service.await;
        unix::restore_blocking_stdin();
        return Ok(match decision {
            Lifecycle::HandOver(executable) => {
                let resume = serde_json::to_string(&ResumeState {
                    pid: std::process::id(),
                    initialize: lock(&tracker).initialize.clone(),
                    carryover: base64::engine::general_purpose::STANDARD.encode(&carryover),
                })
                .map_err(|error| {
                    OrbitError::Execution(format!("encode resumed session: {error}"))
                })?;
                tracing::info!(
                    target: "orbit.mcp",
                    executable = %executable.display(),
                    "handing the stdio session over to the installed executable"
                );
                StdioExit::HandOver { executable, resume }
            }
            _ => {
                tracing::info!(target: "orbit.mcp", "yielding to a pending Orbit upgrade");
                StdioExit::Yielded
            }
        });
    }
}

fn is_idle(tracker: &Mutex<Tracker>, undelivered: &AtomicUsize) -> bool {
    undelivered.load(Ordering::SeqCst) == 0 && lock(tracker).pending.is_empty()
}

fn lock(tracker: &Mutex<Tracker>) -> std::sync::MutexGuard<'_, Tracker> {
    tracker
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Delivers forwarded lines to the service, counting each one consumed.
struct LineReader {
    lines: mpsc::Receiver<Vec<u8>>,
    current: Vec<u8>,
    position: usize,
    undelivered: Arc<AtomicUsize>,
}

impl AsyncRead for LineReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        if this.position >= this.current.len() {
            match this.lines.poll_recv(cx) {
                Poll::Ready(Some(line)) => {
                    this.current = line;
                    this.position = 0;
                }
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Pending => return Poll::Pending,
            }
        }
        let remaining = &this.current[this.position..];
        let take = remaining.len().min(buf.remaining());
        buf.put_slice(&remaining[..take]);
        this.position += take;
        if this.position >= this.current.len() {
            this.undelivered.fetch_sub(1, Ordering::SeqCst);
        }
        Poll::Ready(Ok(()))
    }
}

/// Writes one complete stdout record while holding the process stdout lock.
///
/// Tokio's `Stdout::poll_write` copies at most 2 MiB and drops that lock
/// between chunks. A permitted image read is larger than that once its
/// base64 is carried as both an image block and `structuredContent`, so a
/// second writer can insert bytes inside the record. One `write_all` keeps
/// the lock until the record is finished.
fn write_complete_stdout_frame(frame: &[u8]) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    std::io::Write::write_all(&mut stdout, frame)?;
    std::io::Write::flush(&mut stdout)
}

/// Stdout half that emits each encoded frame with [`write_complete_stdout_frame`].
///
/// `poll_write` only buffers. While a frame write is in flight it accepts
/// nothing and returns [`Poll::Pending`], waking when that write finishes.
/// `poll_flush` returns ready only after every byte already accepted has
/// been emitted, including bytes buffered before a cancelled flush was
/// polled again. A successful flush therefore cannot retire a response
/// whose bytes are still sitting in `staged`.
struct CompleteFrameStdout {
    staged: Vec<u8>,
    flush: Option<tokio::task::JoinHandle<std::io::Result<()>>>,
}

impl CompleteFrameStdout {
    fn new() -> Self {
        Self {
            staged: Vec::new(),
            flush: None,
        }
    }

    /// `true` when this poll finished writing every accepted byte. An idle
    /// flush is `Ok(false)` so a caller does not treat an earlier failure
    /// as answered.
    fn poll_flush_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<bool>> {
        let this = self.get_mut();
        poll_frame_flush(&mut this.staged, &mut this.flush, cx)
    }
}

/// Drives the in-flight frame write. `Ok(false)` means nothing was in flight.
fn poll_in_flight_frame(
    flush: &mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    let polled = {
        let Some(handle) = flush.as_mut() else {
            return Poll::Ready(Ok(false));
        };
        std::future::Future::poll(std::pin::Pin::new(handle), cx)
    };
    match polled {
        Poll::Pending => Poll::Pending,
        Poll::Ready(result) => {
            *flush = None;
            match result {
                Ok(Ok(())) => Poll::Ready(Ok(true)),
                Ok(Err(error)) => Poll::Ready(Err(error)),
                Err(error) => Poll::Ready(Err(std::io::Error::other(error))),
            }
        }
    }
}

fn poll_frame_flush(
    staged: &mut Vec<u8>,
    flush: &mut Option<tokio::task::JoinHandle<std::io::Result<()>>>,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    let mut wrote = false;
    loop {
        match poll_in_flight_frame(flush, cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(finished)) => wrote |= finished,
        }
        // Ready clears the handle. A flush still in flight made no progress.
        if flush.is_some() {
            return Poll::Ready(Err(std::io::Error::other(
                "stdio frame flush lost its in-flight write",
            )));
        }
        if staged.is_empty() {
            return Poll::Ready(Ok(wrote));
        }
        // Bytes accepted while the previous frame was in flight. Emit them
        // before reporting success, or a cancelled flush would retire a
        // response that has not been written.
        let frame = std::mem::take(staged);
        *flush = Some(tokio::task::spawn_blocking(move || {
            write_complete_stdout_frame(&frame)
        }));
    }
}

impl AsyncWrite for CompleteFrameStdout {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // Do not accept bytes into `staged` while a frame is in flight.
        // Completing that write and then reporting flush success would
        // leave the new bytes unemitted. Drive the in-flight write so
        // this waker runs when it finishes; accept the buffer only after
        // that write has released the frame.
        if self.flush.is_some() {
            match poll_in_flight_frame(&mut self.flush, cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Ready(Ok(_)) => {}
            }
        }
        self.staged.extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.poll_flush_frame(cx) {
            Poll::Ready(Ok(_)) => Poll::Ready(Ok(())),
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}

/// Passes the service's output through, retiring each answered request
/// once its response frame has been written.
///
/// A line is recorded only for the prefix [`CompleteFrameStdout`] actually
/// accepts. A `Pending` write accepts nothing, so the retry is the first
/// time those bytes are tracked.
struct TrackingWriter {
    inner: CompleteFrameStdout,
    line: Vec<u8>,
    answered: Vec<Vec<u8>>,
    tracker: Arc<Mutex<Tracker>>,
}

impl AsyncWrite for TrackingWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let accepted = match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
            Poll::Ready(Ok(accepted)) => accepted,
        };
        let this = &mut *self;
        for byte in buf.iter().take(accepted) {
            if *byte == b'\n' {
                this.answered.push(std::mem::take(&mut this.line));
            } else {
                this.line.push(*byte);
            }
        }
        Poll::Ready(Ok(accepted))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_flush_frame(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Ready(Ok(false)) => Poll::Ready(Ok(())),
            Poll::Ready(Ok(true)) => {
                let mut tracker = lock(&this.tracker);
                for line in this.answered.drain(..) {
                    tracker.answered(&line);
                }
                Poll::Ready(Ok(()))
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[cfg(unix)]
mod unix {
    //! Reading fd 0 without buffering past one partial line.

    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::io::unix::AsyncFd;
    use tokio::sync::{mpsc, oneshot};

    use super::{
        MAX_CARRYOVER, MAX_STDIO_LINE, READ_CHUNK, Tracker, lock, rmcp_receive_would_reject,
        write_stdio_invalid_request,
    };

    pub(super) struct StdinFd(OwnedFd);

    impl AsRawFd for StdinFd {
        fn as_raw_fd(&self) -> RawFd {
            self.0.as_raw_fd()
        }
    }

    impl StdinFd {
        fn duplicate() -> std::io::Result<Self> {
            let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: a successful F_DUPFD_CLOEXEC returns a new descriptor
            // owned by the caller.
            Ok(Self(unsafe { OwnedFd::from_raw_fd(fd) }))
        }
    }

    pub(super) struct NonBlockingStdin(AsyncFd<StdinFd>);

    impl NonBlockingStdin {
        /// `None` when stdin cannot be polled (a regular file, `/dev/null`).
        pub(super) fn new() -> Option<Self> {
            set_nonblocking(true)?;
            let fd = match StdinFd::duplicate() {
                Ok(fd) => fd,
                Err(_) => {
                    set_nonblocking(false);
                    return None;
                }
            };
            // SAFETY: `fd` owns its duplicated descriptor. This module keeps
            // it open and unchanged for the lifetime of the registration.
            match unsafe { AsyncFd::register(fd) } {
                Ok(fd) => Some(Self(fd)),
                Err(_) => {
                    set_nonblocking(false);
                    None
                }
            }
        }
    }

    /// Whether [`NonBlockingStdin::new`] would succeed, asked before any
    /// runtime exists: register stdin with a throwaway reactor the same way.
    pub(super) fn stdin_is_pollable() -> bool {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
        else {
            return false;
        };
        let _entered = runtime.enter();
        let Ok(fd) = StdinFd::duplicate() else {
            return false;
        };
        // SAFETY: `fd` owns its duplicated descriptor and remains open and
        // unchanged until the temporary registration is dropped below.
        unsafe { AsyncFd::register(fd) }.is_ok()
    }

    fn set_nonblocking(enabled: bool) -> Option<()> {
        // SAFETY: fcntl on the process's own stdin descriptor with integer
        // flag arguments; no memory is passed.
        let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
        if flags < 0 {
            return None;
        }
        let flags = if enabled {
            flags | libc::O_NONBLOCK
        } else {
            flags & !libc::O_NONBLOCK
        };
        // SAFETY: as above.
        (unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, flags) } >= 0).then_some(())
    }

    /// Hand stdin to the next image the way this one received it.
    pub(super) fn restore_blocking_stdin() {
        let _ = set_nonblocking(false);
    }

    pub(super) enum PumpOutcome {
        /// Stopped on request, holding the partial line read so far and the
        /// sender that keeps the service's input open.
        Stopped {
            carryover: Vec<u8>,
            input: mpsc::Sender<Vec<u8>>,
        },
        /// The client closed stdin.
        Closed,
    }

    pub(super) enum PumpStop {
        /// Stop regardless of the partial line, for closure or yielding.
        Always,
        /// Stop only if the partial line fits in the resumed session state.
        HandOver(oneshot::Sender<bool>),
    }

    /// Forward complete lines from stdin until stopped or closed.
    pub(super) async fn pump(
        stdin: NonBlockingStdin,
        mut buffer: Vec<u8>,
        lines: mpsc::Sender<Vec<u8>>,
        tracker: Arc<Mutex<Tracker>>,
        undelivered: Arc<AtomicUsize>,
        mut stopped: mpsc::Receiver<PumpStop>,
    ) -> std::io::Result<PumpOutcome> {
        // Read the same owned duplicate that Tokio registered for readiness.
        let fd = stdin.0.get_ref().as_raw_fd();
        if !forward_lines(&mut buffer, &lines, &tracker, &undelivered).await {
            return Ok(PumpOutcome::Closed);
        }
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            let mut ready = tokio::select! {
                biased;
                request = stopped.recv() => {
                    if let Some(PumpStop::HandOver(accepted)) = request {
                        if buffer.len() > MAX_CARRYOVER {
                            let _ = accepted.send(false);
                            continue;
                        }
                        let _ = accepted.send(true);
                    }
                    return Ok(PumpOutcome::Stopped {
                        carryover: buffer,
                        input: lines,
                    });
                }
                ready = stdin.0.readable() => ready?,
            };
            let read = ready.try_io(|_| {
                // SAFETY: `stdin` owns `fd` for the full pump and this buffer
                // is writable for its length.
                let read = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
                if read < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(read as usize)
                }
            });
            let read = match read {
                Ok(Ok(read)) => read,
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Ok(Err(error)) => return Err(error),
                Err(_would_block) => continue,
            };
            if read == 0 {
                if !buffer.is_empty() {
                    let line = std::mem::take(&mut buffer);
                    undelivered.fetch_add(1, Ordering::SeqCst);
                    let _ = lines.send(line).await;
                }
                return Ok(PumpOutcome::Closed);
            }
            // Check each line before appending or parsing it. A read may
            // contain multiple lines, so a chunk-wide length check would
            // incorrectly reject several small requests.
            let mut remaining = &chunk[..read];
            while !remaining.is_empty() {
                let end = remaining.iter().position(|byte| *byte == b'\n');
                let take = end.map_or(remaining.len(), |index| index + 1);
                if buffer.len() + take > MAX_STDIO_LINE {
                    tracing::warn!(target: "orbit.mcp", "closing stdio session: input line exceeds the transport limit");
                    return Ok(PumpOutcome::Closed);
                }
                buffer.extend_from_slice(&remaining[..take]);
                remaining = &remaining[take..];
                if end.is_some()
                    && !forward_lines(&mut buffer, &lines, &tracker, &undelivered).await
                {
                    return Ok(PumpOutcome::Closed);
                }
            }
        }
    }

    /// Forward every complete line in `buffer`, leaving a partial one. False
    /// when the service stopped listening.
    async fn forward_lines(
        buffer: &mut Vec<u8>,
        lines: &mpsc::Sender<Vec<u8>>,
        tracker: &Mutex<Tracker>,
        undelivered: &AtomicUsize,
    ) -> bool {
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            // Answer here, before rmcp reads the line. `receive` clears the
            // line and then sends this error from a `select!` branch that a
            // ready response task cancels. The frame uses the service's stdout
            // writer, so it stays a whole record beside a large response. Do
            // not count the line: the service never reads it, and a pending
            // count would stall handover.
            if rmcp_receive_would_reject(&line) {
                if !write_stdio_invalid_request().await {
                    return false;
                }
                continue;
            }
            // Record the request before the service can answer it.
            lock(tracker).forwarded(&line);
            undelivered.fetch_add(1, Ordering::SeqCst);
            if lines.send(line).await.is_err() {
                return false;
            }
        }
        true
    }
}
