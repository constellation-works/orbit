use std::io::{self, Read};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::atomic::AtomicBool;

use super::capture::SharedOutputCapture;

pub(super) const OUTPUT_READER_JOIN_TIMEOUT: Duration = Duration::from_millis(500);
/// How often a pipe worker without a wakeup fd rechecks its cancel flag.
#[cfg(unix)]
const CANCEL_FLAG_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Upper bound on bytes a cancelled reader drains. Matches Linux's default
/// `/proc/sys/fs/pipe-max-size`, the largest buffer an unprivileged writer
/// can request for a pipe.
#[cfg(unix)]
const POST_CANCEL_DRAIN_LIMIT_BYTES: usize = 1024 * 1024;

const OUTPUT_LINE_EVENT_LIMIT_BYTES: usize = 64 * 1024;

#[cfg(unix)]
pub(super) type CancelPairHook<'a> = &'a dyn Fn() -> io::Result<(UnixStream, UnixStream)>;

pub(super) struct OutputReaderContext {
    pub(super) provider: String,
    pub(super) stream: &'static str,
    pub(super) job_run_id: String,
    pub(super) task_id: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) dispatch: tracing::Dispatch,
}

pub(super) struct OutputReaderHandle {
    finished: mpsc::Receiver<()>,
    join: thread::JoinHandle<()>,
    #[cfg(unix)]
    cancel: PipeCancel,
}

/// Supervisor-owned half of a pipe worker's cancellation. The flag is always
/// present; the stream only wakes a worker parked in `poll` sooner.
#[cfg(unix)]
pub(super) struct PipeCancel {
    requested: Arc<AtomicBool>,
    wakeup: Option<UnixStream>,
}

#[cfg(unix)]
impl PipeCancel {
    pub(super) fn cancel(self) {
        self.requested.store(true, Ordering::Release);
        // Closing our end makes the worker's end readable (EOF).
        drop(self.wakeup);
    }
}

/// Worker-owned half of [`PipeCancel`].
#[cfg(unix)]
pub(super) struct CancelWatch {
    requested: Arc<AtomicBool>,
    wakeup: Option<UnixStream>,
}

struct LiveReaderGuard {
    counter: Option<Arc<AtomicUsize>>,
}

impl LiveReaderGuard {
    fn enter(counter: Option<Arc<AtomicUsize>>) -> Self {
        if let Some(counter) = counter.as_ref() {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        Self { counter }
    }
}

impl Drop for LiveReaderGuard {
    fn drop(&mut self) {
        if let Some(counter) = self.counter.as_ref() {
            counter.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

#[cfg(unix)]
pub(super) fn pipe_cancellation(
    cancel_pair: Option<CancelPairHook<'_>>,
) -> (PipeCancel, CancelWatch) {
    let pair_result = cancel_pair.map_or_else(UnixStream::pair, |make_pair| make_pair());
    // Without a pair the worker falls back to timed polls of the cancel flag.
    let (wakeup, cancel_wakeup) = match pair_result {
        Ok((wakeup, cancel)) => (Some(wakeup), Some(cancel)),
        Err(_) => (None, None),
    };
    let requested = Arc::new(AtomicBool::new(false));
    (
        PipeCancel {
            requested: Arc::clone(&requested),
            wakeup: cancel_wakeup,
        },
        CancelWatch { requested, wakeup },
    )
}

#[cfg(unix)]
pub(super) fn spawn_output_reader<R>(
    handle: R,
    buf: SharedOutputCapture,
    context: OutputReaderContext,
    live_readers: Option<Arc<AtomicUsize>>,
    cancel_pair: Option<CancelPairHook<'_>>,
) -> OutputReaderHandle
where
    R: Read + IntoRawFd + Send + 'static,
{
    let fd = handle.into_raw_fd();
    // SAFETY: `into_raw_fd` transferred ownership of a valid pipe descriptor.
    let mut reader = unsafe { File::from_raw_fd(fd) };
    // The reader only calls `read` after `poll` reports the pipe ready, so a
    // failure here cannot turn it into a blocking reader.
    let _ = set_nonblocking(reader.as_raw_fd());
    let (cancel, watch) = pipe_cancellation(cancel_pair);

    // One reader sends one completion signal; capacity one cannot block it.
    let (finished_tx, finished) = mpsc::sync_channel(1);
    let join = thread::spawn(move || {
        let _live = LiveReaderGuard::enter(live_readers);
        tracing::dispatcher::with_default(&context.dispatch, || {
            read_cancelable_output(&mut reader, &watch, &buf, &context);
        });
        let _ = finished_tx.send(());
    });
    OutputReaderHandle {
        finished,
        join,
        cancel,
    }
}

#[cfg(not(unix))]
pub(super) fn spawn_output_reader<R>(
    handle: R,
    buf: SharedOutputCapture,
    context: OutputReaderContext,
    live_readers: Option<Arc<AtomicUsize>>,
) -> OutputReaderHandle
where
    R: Read + Send + 'static,
{
    // One reader sends one completion signal; capacity one cannot block it.
    let (finished_tx, finished) = mpsc::sync_channel(1);
    let join = thread::spawn(move || {
        let _live = LiveReaderGuard::enter(live_readers);
        tracing::dispatcher::with_default(&context.dispatch, || {
            read_blocking_output(handle, &buf, &context);
        });
        let _ = finished_tx.send(());
    });
    OutputReaderHandle { finished, join }
}

pub(super) fn join_output_reader(reader: OutputReaderHandle, deadline: Instant) {
    let OutputReaderHandle {
        finished,
        join,
        #[cfg(unix)]
        cancel,
    } = reader;
    let timeout = deadline.saturating_duration_since(Instant::now());
    match finished.recv_timeout(timeout) {
        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = join.join();
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // A cancelled Unix reader never blocks in `read` and drains a
            // bounded byte count, so this join is bounded too.
            #[cfg(unix)]
            {
                cancel.cancel();
                let _ = join.join();
            }
            #[cfg(not(unix))]
            drop(join);
        }
    }
}

#[cfg(not(unix))]
fn read_blocking_output<R: Read>(
    mut reader: R,
    buf: &SharedOutputCapture,
    context: &OutputReaderContext,
) {
    let mut chunk = [0u8; 4096];
    let mut line_buf = Vec::new();
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => append_output_chunk(buf, context, &chunk[..n], &mut line_buf),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    flush_line_buf(context, &line_buf);
}

#[cfg(unix)]
fn read_cancelable_output(
    reader: &mut File,
    watch: &CancelWatch,
    buf: &SharedOutputCapture,
    context: &OutputReaderContext,
) {
    let mut chunk = [0u8; 4096];
    let mut line_buf = Vec::new();
    let mut cancelled = false;
    loop {
        match poll_pipe_or_cancel(reader.as_raw_fd(), libc::POLLIN, watch) {
            PollOutcome::Failed => break,
            PollOutcome::Cancelled => {
                cancelled = true;
                break;
            }
            PollOutcome::Ready => match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => append_output_chunk(buf, context, &chunk[..n], &mut line_buf),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => continue,
                Err(_) => break,
            },
        }
    }
    if cancelled {
        drain_readable_output(reader, buf, context, &mut line_buf);
    }
    flush_line_buf(context, &line_buf);
}

fn append_output_chunk(
    buf: &SharedOutputCapture,
    context: &OutputReaderContext,
    raw: &[u8],
    line_buf: &mut Vec<u8>,
) {
    buf.lock().unwrap_or_else(PoisonError::into_inner).push(raw);
    emit_output_chunk(
        &context.provider,
        context.stream,
        &context.job_run_id,
        context.task_id.as_deref(),
        context.cwd.as_deref(),
        raw,
        line_buf,
    );
}

fn flush_line_buf(context: &OutputReaderContext, line_buf: &[u8]) {
    if line_buf.is_empty()
        || !tracing::enabled!(target: "orbit_engine::activity_job::cli_runner::supervisor", tracing::Level::INFO)
    {
        return;
    }
    emit_output_line(
        &context.provider,
        context.stream,
        &context.job_run_id,
        context.task_id.as_deref(),
        context.cwd.as_deref(),
        line_buf,
    );
}

#[cfg(unix)]
fn drain_readable_output(
    reader: &mut File,
    buf: &SharedOutputCapture,
    context: &OutputReaderContext,
    line_buf: &mut Vec<u8>,
) {
    // Snapshot the queued byte count once: bytes written after the cancel
    // belong to no invocation and must not keep this loop alive.
    let mut remaining = queued_bytes(reader.as_raw_fd())
        .map_or(POST_CANCEL_DRAIN_LIMIT_BYTES, |queued| {
            queued.min(POST_CANCEL_DRAIN_LIMIT_BYTES)
        });
    let mut chunk = [0u8; 4096];
    while remaining > 0 {
        if !fd_is_readable(reader.as_raw_fd()) {
            return;
        }
        let want = remaining.min(chunk.len());
        match reader.read(&mut chunk[..want]) {
            Ok(0) => return,
            Ok(n) => {
                remaining = remaining.saturating_sub(n);
                append_output_chunk(buf, context, &chunk[..n], line_buf);
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
fn queued_bytes(fd: RawFd) -> Option<usize> {
    let mut available: libc::c_int = 0;
    // SAFETY: FIONREAD writes one `c_int` through a valid pointer; `fd` is the
    // reader thread's own pipe.
    let rc = unsafe { libc::ioctl(fd, libc::FIONREAD, &mut available) };
    if rc < 0 {
        return None;
    }
    usize::try_from(available).ok()
}

#[cfg(unix)]
pub(super) enum PollOutcome {
    Ready,
    Cancelled,
    Failed,
}

#[cfg(unix)]
pub(super) fn poll_pipe_or_cancel(
    pipe_fd: RawFd,
    events: libc::c_short,
    watch: &CancelWatch,
) -> PollOutcome {
    let mut fds = [
        libc::pollfd {
            fd: pipe_fd,
            events,
            revents: 0,
        },
        libc::pollfd {
            fd: watch.wakeup.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // With a wakeup fd the worker can park indefinitely; without one it must
    // wake periodically to observe the cancel flag.
    let (nfds, timeout_ms) = if watch.wakeup.is_some() {
        (2, -1)
    } else {
        (1, CANCEL_FLAG_POLL_INTERVAL.as_millis() as libc::c_int)
    };
    loop {
        // The flag is checked on every pass so a continuously ready pipe
        // cannot starve cancellation.
        if watch.requested.load(Ordering::Acquire) {
            return PollOutcome::Cancelled;
        }
        // SAFETY: `fds` is a valid pollfd array we own for the duration of
        // the call and `nfds` never exceeds its length; both descriptors are
        // owned by this thread.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), nfds, timeout_ms) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return PollOutcome::Failed;
        }
        if fds[1].revents != 0 {
            return PollOutcome::Cancelled;
        }
        if fds[0].revents != 0 {
            return PollOutcome::Ready;
        }
    }
}

#[cfg(unix)]
fn fd_is_readable(fd: RawFd) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is a valid pollfd we own; `fd` is the reader thread's pipe.
    let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
    rc > 0 && pfd.revents != 0
}

#[cfg(unix)]
pub(super) fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is an owned descriptor; F_GETFL reads flags only.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is still owned; F_SETFL only adds O_NONBLOCK.
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn emit_output_chunk(
    provider: &str,
    stream: &str,
    job_run_id: &str,
    task_id: Option<&str>,
    cwd: Option<&str>,
    raw: &[u8],
    line_buf: &mut Vec<u8>,
) {
    if !tracing::enabled!(target: "orbit_engine::activity_job::cli_runner::supervisor", tracing::Level::INFO)
    {
        return;
    }

    for segment in raw.split_inclusive(|byte| *byte == b'\n') {
        line_buf.extend_from_slice(segment);
        if segment.ends_with(b"\n") || line_buf.len() >= OUTPUT_LINE_EVENT_LIMIT_BYTES {
            emit_output_line(provider, stream, job_run_id, task_id, cwd, line_buf);
            line_buf.clear();
        }
    }
}

fn emit_output_line(
    provider: &str,
    stream: &str,
    job_run_id: &str,
    task_id: Option<&str>,
    cwd: Option<&str>,
    raw_line: &[u8],
) {
    let line = line_text(raw_line);
    if let Some(cwd) = cwd {
        tracing::info!(target: "orbit_engine::activity_job::cli_runner::supervisor",
            agent_output = true,
            provider = provider,
            stream = stream,
            job_run_id = job_run_id,
            task_id = task_id,
            cwd = cwd,
            line = line.as_str()
        );
    } else {
        tracing::info!(target: "orbit_engine::activity_job::cli_runner::supervisor",
            agent_output = true,
            provider = provider,
            stream = stream,
            job_run_id = job_run_id,
            task_id = task_id,
            line = line.as_str()
        );
    }
}

fn line_text(raw_line: &[u8]) -> String {
    let line = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    String::from_utf8_lossy(line).into_owned()
}
