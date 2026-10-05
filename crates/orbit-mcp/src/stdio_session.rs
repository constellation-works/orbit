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
//! a pending request, since its error may not echo the original id.
//!
//! At that point it stops reading, stops the service, and returns
//! [`StdioExit::HandOver`] with the client's original `initialize` request
//! and any partial line it had read. The caller execs the new executable with
//! that state in [`RESUME_ENV`]. Because `exec` keeps the pid and the stdio
//! descriptors, the client sees one uninterrupted session. The new process
//! replays the `initialize` request into its server rather than expecting a
//! second handshake.

use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use base64::Engine as _;
use orbit_common::OrbitError;
use orbit_common::fs::generation;
use rmcp::ServiceExt;
use rmcp::model::{
    ClientJsonRpcMessage, ClientNotification, ClientRequest, InitializeRequestParams, Meta,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

use crate::adapter::OrbitToolServer;

/// Carries a resumed session's state into the replacement image.
pub const RESUME_ENV: &str = "ORBIT_MCP_RESUME";

/// How often an idle session checks for a replacement or a pending switch.
const LIFECYCLE_INTERVAL: Duration = Duration::from_secs(2);
/// A partial line longer than this is not handed over; the session keeps
/// serving and retries at a later idle point.
const MAX_CARRYOVER: usize = 32 * 1024;
const READ_CHUNK: usize = 64 * 1024;

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
        // A raw method/id pair is insufficient: decoding failures receive an
        // id-less error or are dropped by the transport's compatibility path.
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

/// Without a pollable stdin (a regular file, `/dev/null`) the session is
/// served as-is and never handed over.
async fn serve_plain(
    server: OrbitToolServer,
    resumed: Option<Resumed>,
) -> Result<StdioExit, OrbitError> {
    use tokio::io::AsyncReadExt as _;
    let (carryover, initialize) = match resumed {
        Some(resumed) => (resumed.carryover, resumed.initialize),
        None => (Vec::new(), None),
    };
    let reader = std::io::Cursor::new(carryover).chain(tokio::io::stdin());
    let transport = (reader, tokio::io::stdout());
    let running = match initialize {
        Some(request) => {
            server
                .apply_initialize(&request, &Meta::default())
                .map_err(|error| start_error(&error))?;
            rmcp::service::serve_directly(server, transport, Some(request))
        }
        None => server
            .serve(transport)
            .await
            .map_err(|error| start_error(&error))?,
    };
    running
        .waiting()
        .await
        .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))?;
    Ok(StdioExit::Closed)
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
    let (stop, stopped) = tokio::sync::watch::channel(false);
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
        inner: tokio::io::stdout(),
        line: Vec::new(),
        answered: Vec::new(),
        tracker: Arc::clone(&tracker),
    };
    let running = match initialize {
        Some(request) => {
            server
                .apply_initialize(&request, &Meta::default())
                .map_err(|error| start_error(&error))?;
            tracing::info!(target: "orbit.mcp", "resumed the stdio session after an executable handover");
            rmcp::service::serve_directly(server, (reader, writer), Some(request))
        }
        None => server
            .serve((reader, writer))
            .await
            .map_err(|error| start_error(&error))?,
    };
    let cancel = running.cancellation_token();
    let mut service = tokio::spawn(running.waiting());
    let mut interval = tokio::time::interval(LIFECYCLE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            finished = &mut service => {
                finished
                    .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))?
                    .map_err(|error| OrbitError::Execution(format!("mcp serve_stdio wait: {error}")))?;
                let _ = stop.send(true);
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
        // Stop reading first, then wait out whatever was forwarded before
        // the pump stopped: a request answered, a notification consumed.
        let _ = stop.send(true);
        let Ok(Ok(outcome)) = pump.await else {
            return Err(OrbitError::Execution(
                "mcp serve_stdio: the stdin reader stopped unexpectedly".into(),
            ));
        };
        let carryover = match outcome {
            unix::PumpOutcome::Stopped(carryover) => carryover,
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
            Lifecycle::HandOver(executable) if carryover.len() <= MAX_CARRYOVER => {
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

/// Passes the service's output through, retiring each answered request
/// once its response has been flushed.
struct TrackingWriter<W> {
    inner: W,
    line: Vec<u8>,
    answered: Vec<Vec<u8>>,
    tracker: Arc<Mutex<Tracker>>,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for TrackingWriter<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        let written = match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(written)) => written,
            other => return other,
        };
        for byte in &buf[..written] {
            if *byte == b'\n' {
                this.answered.push(std::mem::take(&mut this.line));
            } else {
                this.line.push(*byte);
            }
        }
        Poll::Ready(Ok(written))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                let mut tracker = lock(&this.tracker);
                for line in this.answered.drain(..) {
                    tracker.answered(&line);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(unix)]
mod unix {
    //! Reading fd 0 without buffering past one partial line.

    use std::io::Read;
    use std::mem::ManuallyDrop;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::io::unix::AsyncFd;
    use tokio::sync::{mpsc, watch};

    use super::{READ_CHUNK, Tracker, lock};

    pub(super) struct StdinFd;

    impl AsRawFd for StdinFd {
        fn as_raw_fd(&self) -> RawFd {
            libc::STDIN_FILENO
        }
    }

    pub(super) struct NonBlockingStdin(AsyncFd<StdinFd>);

    impl NonBlockingStdin {
        /// `None` when stdin cannot be polled (a regular file, `/dev/null`).
        pub(super) fn new() -> Option<Self> {
            set_nonblocking(true)?;
            match AsyncFd::new(StdinFd) {
                Ok(fd) => Some(Self(fd)),
                Err(_) => {
                    set_nonblocking(false);
                    None
                }
            }
        }
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
        /// Stopped on request, holding the partial line read so far.
        Stopped(Vec<u8>),
        /// The client closed stdin.
        Closed,
    }

    /// Forward complete lines from stdin until stopped or closed.
    pub(super) async fn pump(
        stdin: NonBlockingStdin,
        mut buffer: Vec<u8>,
        lines: mpsc::Sender<Vec<u8>>,
        tracker: Arc<Mutex<Tracker>>,
        undelivered: Arc<AtomicUsize>,
        mut stopped: watch::Receiver<bool>,
    ) -> std::io::Result<PumpOutcome> {
        // Borrow fd 0 unbuffered; it must never be closed from here.
        // SAFETY: fd 0 stays open for the process lifetime and ManuallyDrop
        // keeps this File from closing it.
        let file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(libc::STDIN_FILENO) });
        if !forward_lines(&mut buffer, &lines, &tracker, &undelivered).await {
            return Ok(PumpOutcome::Closed);
        }
        let mut chunk = vec![0u8; READ_CHUNK];
        loop {
            if *stopped.borrow() {
                return Ok(PumpOutcome::Stopped(buffer));
            }
            let mut ready = tokio::select! {
                ready = stdin.0.readable() => ready?,
                _ = stopped.changed() => continue,
            };
            let read = ready.try_io(|_| (&*file).read(&mut chunk));
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
            buffer.extend_from_slice(&chunk[..read]);
            if !forward_lines(&mut buffer, &lines, &tracker, &undelivered).await {
                return Ok(PumpOutcome::Closed);
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
