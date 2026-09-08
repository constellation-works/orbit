// Existing expect calls in this module document local invariants; keep the allow scoped while the workspace lint is ratcheted.
#![allow(clippy::expect_used)]

//! CLI subprocess supervisor.
//!
//! # Output drain / truncation contract
//!
//! stdout/stderr readers belong to the supervisor until it returns. After the
//! child exits, times out, or `wait` fails, the supervisor kills the process
//! tree and then:
//!
//! 1. **Bounded drain.** Readers keep consuming readable bytes and emitting
//!    tracing line events until EOF or [`OUTPUT_READER_JOIN_TIMEOUT`].
//! 2. **Cancel.** If a writer still holds the pipe (an escaped session), the
//!    supervisor wakes each reader through an owned pollable cancel fd. The
//!    reader then nonblocking-drains whatever is already readable and stops
//!    capturing and emitting. The supervisor joins the reader thread before
//!    returning. It does not close another thread's pipe descriptor and does
//!    not treat a duplicate close as cancellation.
//! 3. **Capture finish.** Bytes collected before cancel/EOF are frozen by
//!    [`RollingOutputCapture::finish`]: under the limit they are kept in full;
//!    over the limit the prefix plus a complete-line tail are kept and
//!    `truncated` is set. Writes that arrive after cancel are discarded and
//!    must not be logged for the completed invocation.
//! 4. **Wait error.** Readers are finalized the same way; the function then
//!    returns [`SpawnError`] and drops the finished capture because the error
//!    type has no output payload.
//!
//! Unix implements wakeup with `poll` on the reader fd plus a `UnixStream`
//! pair. Non-Unix platforms keep a blocking `Read` and cannot interrupt an
//! escaped holder; that path is not tested here.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use super::super::dispatcher::ResolvedSandbox;
use super::spawn::{SpawnError, SpawnedChild, spawn_child_with_optional_sandbox};
use orbit_common::process::output_capture::capture_limit_from_env;

/// Default wall-clock timeout when `AgentLoopSpec::wall_clock_timeout_seconds`
/// is zero. Matches §7.6 guidance: CLI subprocesses must have a mandatory
/// wall-clock guard.
pub(super) const DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS: u64 = 300;

pub(super) type SpawnOutput = (CapturedOutput, CapturedOutput, Option<i32>, Duration, bool);

const OUTPUT_READER_JOIN_TIMEOUT: Duration = Duration::from_millis(500);
const CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_ENV: &str = "ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES";
const DEFAULT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;
const OUTPUT_LINE_EVENT_LIMIT_BYTES: usize = 64 * 1024;

type SharedOutputCapture = Arc<Mutex<RollingOutputCapture>>;
type WaitHook<'a> = &'a dyn Fn(&mut Child) -> std::io::Result<Option<ExitStatus>>;
#[cfg(unix)]
type CancelPairHook<'a> = &'a dyn Fn() -> io::Result<(UnixStream, UnixStream)>;

#[derive(Debug)]
pub(super) struct CapturedOutput {
    bytes: Vec<u8>,
    protocol_offset: usize,
    observed_bytes: usize,
    capture_limit_bytes: usize,
    truncated: bool,
}

impl CapturedOutput {
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(super) fn protocol_bytes(&self) -> &[u8] {
        &self.bytes[self.protocol_offset..]
    }

    pub(super) fn observed_bytes(&self) -> usize {
        self.observed_bytes
    }

    pub(super) fn capture_limit_bytes(&self) -> usize {
        self.capture_limit_bytes
    }

    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Debug)]
struct RollingOutputCapture {
    prefix: Vec<u8>,
    tail: VecDeque<u8>,
    observed_bytes: usize,
    limit: usize,
    truncated: bool,
}

impl RollingOutputCapture {
    fn new(limit: usize) -> Self {
        Self {
            prefix: Vec::new(),
            tail: VecDeque::new(),
            observed_bytes: 0,
            limit,
            truncated: false,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.observed_bytes = self.observed_bytes.saturating_add(chunk.len());
        if !self.truncated && self.prefix.len().saturating_add(chunk.len()) <= self.limit {
            self.prefix.extend_from_slice(chunk);
            return;
        }

        let prefix_limit = self.limit / 2;
        let tail_limit = self.limit.saturating_sub(prefix_limit);
        if !self.truncated {
            let displaced = self.prefix.split_off(prefix_limit.min(self.prefix.len()));
            self.tail.extend(displaced);
            self.truncated = true;
        }
        self.tail.extend(chunk);
        while self.tail.len() > tail_limit {
            self.tail.pop_front();
        }
    }

    fn finish(&self) -> CapturedOutput {
        if !self.truncated {
            return CapturedOutput {
                bytes: self.prefix.clone(),
                protocol_offset: 0,
                observed_bytes: self.observed_bytes,
                capture_limit_bytes: self.limit,
                truncated: false,
            };
        }

        // The tail may start in the middle of a structured JSONL event. Drop
        // that partial line so protocol consumers can still parse the final
        // complete provider events (including the Orbit response envelope).
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        let complete_tail = tail
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |idx| &tail[idx + 1..]);
        let marker = format!(
            "\n[orbit: output capture truncated; observed_bytes={}; capture_limit_bytes={}]\n",
            self.observed_bytes, self.limit
        );
        let protocol_offset = self.prefix.len() + marker.len();
        let mut bytes = Vec::with_capacity(protocol_offset + complete_tail.len());
        bytes.extend_from_slice(&self.prefix);
        bytes.extend_from_slice(marker.as_bytes());
        bytes.extend_from_slice(complete_tail);

        CapturedOutput {
            bytes,
            protocol_offset,
            observed_bytes: self.observed_bytes,
            capture_limit_bytes: self.limit,
            truncated: true,
        }
    }
}

pub(super) struct SpawnTraceContext<'a> {
    pub(super) provider: &'a str,
    pub(super) job_run_id: &'a str,
    pub(super) task_id: Option<&'a str>,
    pub(super) cwd: Option<&'a str>,
}

pub(super) struct SpawnWithTimeoutRequest<'a> {
    pub(super) program: &'a str,
    pub(super) args: &'a [String],
    pub(super) stdin_bytes: &'a [u8],
    pub(super) env: &'a [(String, String)],
    pub(super) cwd: Option<&'a Path>,
    pub(super) timeout: Duration,
    pub(super) sandbox: Option<&'a ResolvedSandbox>,
    pub(super) trace: SpawnTraceContext<'a>,
    pub(super) output_capture_limit: Option<usize>,
    /// [ORB-10496] Invoked once with the spawned child's PID, immediately after
    /// spawn and before the supervision loop. The PID is otherwise visible only
    /// inside this module (process-group cleanup), so a long-running provider
    /// child has no observable identity while it runs.
    pub(super) on_spawn: Option<&'a dyn Fn(u32)>,
    /// Test seam for exercising wait failures without depending on another
    /// thread reaping the child between `try_wait` calls.
    pub(super) wait: Option<WaitHook<'a>>,
    /// Test seam: each live output reader increments this counter for the
    /// lifetime of its thread so tests can observe finalization without
    /// sampling process-wide thread counts.
    pub(super) live_readers: Option<Arc<AtomicUsize>>,
    /// Test seam for exercising output capture when the pollable cancellation
    /// channel cannot be constructed.
    #[cfg(unix)]
    pub(super) cancel_pair: Option<CancelPairHook<'a>>,
}

struct OutputReaderContext {
    provider: String,
    stream: &'static str,
    job_run_id: String,
    task_id: Option<String>,
    cwd: Option<String>,
    dispatch: tracing::Dispatch,
}

struct OutputReaderHandle {
    finished: mpsc::Receiver<()>,
    join: thread::JoinHandle<()>,
    #[cfg(unix)]
    cancel: Option<UnixStream>,
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

pub(super) fn spawn_with_timeout(
    request: SpawnWithTimeoutRequest<'_>,
) -> Result<SpawnOutput, SpawnError> {
    let SpawnWithTimeoutRequest {
        program,
        args,
        stdin_bytes,
        env,
        cwd,
        timeout,
        sandbox,
        trace,
        output_capture_limit,
        on_spawn,
        wait,
        live_readers,
        #[cfg(unix)]
        cancel_pair,
    } = request;

    let started = Instant::now();
    let SpawnedChild {
        mut child,
        // The temp profile must outlive the child — drop it after wait.
        _profile_temp,
    } = spawn_child_with_optional_sandbox(program, args, env, cwd, sandbox, trace.provider)
        .map_err(|err| SpawnError {
            permanent: err.permanent,
            message: format!("spawn {program}: {}", err.message),
        })?;

    // Report the PID before any blocking work: the whole point is to be
    // observable during a long invocation, and the child is already running.
    if let Some(on_spawn) = on_spawn {
        on_spawn(child.id());
    }

    if let Some(mut stdin) = child.stdin.take() {
        let bytes = stdin_bytes.to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }

    let output_limit = output_capture_limit.unwrap_or_else(default_output_capture_limit);
    let stdout_buf = Arc::new(Mutex::new(RollingOutputCapture::new(output_limit)));
    let stderr_buf = Arc::new(Mutex::new(RollingOutputCapture::new(output_limit)));
    let dispatch = tracing::dispatcher::get_default(Clone::clone);

    let stdout_reader = child.stdout.take().map(|handle| {
        spawn_output_reader(
            handle,
            Arc::clone(&stdout_buf),
            OutputReaderContext {
                provider: trace.provider.to_string(),
                stream: "stdout",
                job_run_id: trace.job_run_id.to_string(),
                task_id: trace.task_id.map(ToString::to_string),
                cwd: trace.cwd.map(ToString::to_string),
                dispatch: dispatch.clone(),
            },
            live_readers.clone(),
            #[cfg(unix)]
            cancel_pair,
        )
    });
    let stderr_reader = child.stderr.take().map(|handle| {
        spawn_output_reader(
            handle,
            Arc::clone(&stderr_buf),
            OutputReaderContext {
                provider: trace.provider.to_string(),
                stream: "stderr",
                job_run_id: trace.job_run_id.to_string(),
                task_id: trace.task_id.map(ToString::to_string),
                cwd: trace.cwd.map(ToString::to_string),
                dispatch,
            },
            live_readers,
            #[cfg(unix)]
            cancel_pair,
        )
    });

    let mut timed_out = false;
    let deadline = started + timeout;
    let wait_result = loop {
        let result = match wait {
            Some(wait) => wait(&mut child),
            None => child.try_wait(),
        };
        match result {
            Ok(Some(status)) => {
                break Ok(Some(status));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    break Ok(None);
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(err) => {
                break Err(err);
            }
        }
    };

    let (exit_status, wait_error) = match wait_result {
        Ok(exit_status) => (exit_status, None),
        // `wait` failures are host-side and not clearly deterministic — leave
        // them retryable after the common cleanup below.
        Err(err) => (None, Some(err)),
    };

    kill_child_process_tree(&mut child);

    // The join is bounded on every exit path, not only after a timeout. A
    // reader returns when the last writer closes the pipe, and a helper the
    // agent left in its own session (`setsid`) keeps the write end open after
    // the child itself exits and after the group kill above. An unbounded
    // join there never returns, no finish event is emitted, and the run's
    // reservation is never released.
    let reader_join_deadline = Instant::now() + OUTPUT_READER_JOIN_TIMEOUT;
    if let Some(h) = stdout_reader {
        join_output_reader(h, reader_join_deadline);
    }
    if let Some(h) = stderr_reader {
        join_output_reader(h, reader_join_deadline);
    }

    let stdout = finish_captured_output(&stdout_buf, output_limit);
    let stderr = finish_captured_output(&stderr_buf, output_limit);

    if let Some(err) = wait_error {
        drop((stdout, stderr));
        return Err(SpawnError::transient(format!("wait {program}: {err}")));
    }

    let exit_code = exit_status.as_ref().and_then(|s| s.code());
    let duration = started.elapsed();
    Ok((stdout, stderr, exit_code, duration, timed_out))
}

fn finish_captured_output(buf: &SharedOutputCapture, output_limit: usize) -> CapturedOutput {
    buf.lock()
        .map(|buf| buf.finish())
        .unwrap_or_else(|_| CapturedOutput {
            bytes: Vec::new(),
            protocol_offset: 0,
            observed_bytes: 0,
            capture_limit_bytes: output_limit,
            truncated: false,
        })
}

fn default_output_capture_limit() -> usize {
    capture_limit_from_env(
        CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_ENV,
        DEFAULT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES,
    )
}

#[cfg(unix)]
fn spawn_output_reader<R>(
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
    let pair_result = cancel_pair.map_or_else(UnixStream::pair, |make_pair| make_pair());
    let (wakeup, cancel) = match pair_result {
        Ok((wakeup, cancel)) => {
            // The fallback below uses a blocking read loop, so only make the
            // pipe nonblocking when its pollable cancel channel exists.
            let _ = set_nonblocking(reader.as_raw_fd());
            let _ = wakeup.set_nonblocking(true);
            let _ = cancel.set_nonblocking(true);
            (Some(wakeup), Some(cancel))
        }
        Err(_) => (None, None),
    };

    let (finished_tx, finished) = mpsc::channel();
    let join = thread::spawn(move || {
        let _live = LiveReaderGuard::enter(live_readers);
        tracing::dispatcher::with_default(&context.dispatch, || {
            read_cancelable_output(&mut reader, wakeup.as_ref(), &buf, &context);
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
fn spawn_output_reader<R>(
    handle: R,
    buf: SharedOutputCapture,
    context: OutputReaderContext,
    live_readers: Option<Arc<AtomicUsize>>,
) -> OutputReaderHandle
where
    R: Read + Send + 'static,
{
    let (finished_tx, finished) = mpsc::channel();
    let join = thread::spawn(move || {
        let _live = LiveReaderGuard::enter(live_readers);
        tracing::dispatcher::with_default(&context.dispatch, || {
            read_blocking_output(handle, &buf, &context);
        });
        let _ = finished_tx.send(());
    });
    OutputReaderHandle { finished, join }
}

fn join_output_reader(reader: OutputReaderHandle, deadline: Instant) {
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
            #[cfg(unix)]
            {
                drop(cancel);
                let _ = join.join();
            }
            #[cfg(not(unix))]
            drop(join);
        }
    }
}

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
    wakeup: Option<&UnixStream>,
    buf: &SharedOutputCapture,
    context: &OutputReaderContext,
) {
    let Some(wakeup) = wakeup else {
        read_blocking_output(reader, buf, context);
        return;
    };

    let mut chunk = [0u8; 4096];
    let mut line_buf = Vec::new();
    let mut cancelled = false;
    loop {
        match poll_reader_or_cancel(reader.as_raw_fd(), wakeup.as_raw_fd()) {
            PollOutcome::Failed => break,
            PollOutcome::Cancelled => {
                cancelled = true;
                break;
            }
            PollOutcome::Readable => match reader.read(&mut chunk) {
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
    buf.lock()
        .expect("subprocess output buf poisoned")
        .push(raw);
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
    if line_buf.is_empty() {
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
    let mut chunk = [0u8; 4096];
    loop {
        if !fd_is_readable(reader.as_raw_fd()) {
            return;
        }
        match reader.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => append_output_chunk(buf, context, &chunk[..n], line_buf),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return,
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
enum PollOutcome {
    Readable,
    Cancelled,
    Failed,
}

#[cfg(unix)]
fn poll_reader_or_cancel(reader_fd: RawFd, wakeup_fd: RawFd) -> PollOutcome {
    let mut fds = [
        libc::pollfd {
            fd: reader_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wakeup_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: `fds` is a valid two-element pollfd array we own for the
        // duration of the call; both descriptors are owned by this thread.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
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
            return PollOutcome::Readable;
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
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
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

fn kill_child_process_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let _ = signal_child_process_group(child.id(), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn signal_child_process_group(child_id: u32, signal: libc::c_int) -> std::io::Result<()> {
    if child_id == 0 || child_id > i32::MAX as u32 {
        return Ok(());
    }
    let rc = unsafe { libc::killpg(child_id as libc::pid_t, signal) };
    if rc == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
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
        tracing::info!(
            provider = provider,
            stream = stream,
            job_run_id = job_run_id,
            task_id = task_id,
            cwd = cwd,
            line = line.as_str()
        );
    } else {
        tracing::info!(
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
