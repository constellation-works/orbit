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
//!    supervisor sets each reader's cancel flag and wakes it through an owned
//!    pollable cancel fd. The reader then drains at most the bytes already
//!    queued in the pipe when it observed the cancel (capped at
//!    [`POST_CANCEL_DRAIN_LIMIT_BYTES`]), so a writer that keeps producing
//!    cannot extend the drain, and stops capturing and emitting. The
//!    supervisor joins the reader thread before returning. It does not close
//!    another thread's pipe descriptor and does not treat a duplicate close
//!    as cancellation.
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
//! pair. When the pair cannot be created (for example `EMFILE`), the reader
//! still never blocks in `read`: it polls the pipe with a
//! [`CANCEL_FLAG_POLL_INTERVAL`] timeout and rechecks the cancel flag, so
//! finalization stays bounded without a wakeup fd. On Unix the supervisor
//! therefore returns within [`OUTPUT_READER_JOIN_TIMEOUT`] of process-tree
//! cleanup plus one poll interval and one bounded drain, whatever an escaped
//! writer does. Non-Unix platforms keep a blocking `Read` and cannot
//! interrupt an escaped holder; that path is not tested here.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, ExitStatus};
#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
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
use wait_timeout::ChildExt;

/// Default wall-clock timeout when `AgentLoopSpec::wall_clock_timeout_seconds`
/// is zero. Matches §7.6 guidance: CLI subprocesses must have a mandatory
/// wall-clock guard.
pub(super) const DEFAULT_WALL_CLOCK_TIMEOUT_SECONDS: u64 = 300;

type SpawnOutput = (CapturedOutput, CapturedOutput, Option<i32>, Duration, bool);

const OUTPUT_READER_JOIN_TIMEOUT: Duration = Duration::from_millis(500);
/// How often a reader without a wakeup fd rechecks its cancel flag.
#[cfg(unix)]
const CANCEL_FLAG_POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Upper bound on bytes a cancelled reader drains. Matches Linux's default
/// `/proc/sys/fs/pipe-max-size`, the largest buffer an unprivileged writer
/// can request for a pipe.
#[cfg(unix)]
const POST_CANCEL_DRAIN_LIMIT_BYTES: usize = 1024 * 1024;
const PROCESS_GROUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);
const CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_ENV: &str = "ORBIT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES";
const DEFAULT_CLI_RUNNER_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;
const OUTPUT_LINE_EVENT_LIMIT_BYTES: usize = 64 * 1024;
/// Newest stdout bytes a progress sample carries. Large enough for one
/// provider event holding a long assistant message.
const PROGRESS_WINDOW_BYTES: usize = 64 * 1024;

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

    /// The newest complete lines of the capture, at most `window` bytes.
    fn recent(&self, window: usize) -> Vec<u8> {
        let (bytes, from_start) = if self.truncated {
            let skip = self.tail.len().saturating_sub(window);
            (
                self.tail.iter().skip(skip).copied().collect::<Vec<_>>(),
                false,
            )
        } else {
            let start = self.prefix.len().saturating_sub(window);
            (self.prefix[start..].to_vec(), start == 0)
        };
        if from_start {
            return bytes;
        }
        // The window may open mid-line; that fragment is not a line.
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or_else(Vec::new, |idx| bytes[idx + 1..].to_vec())
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

/// [ORB-13899] What a running child has written to stdout so far.
pub(super) struct OutputProgress {
    pub(super) observed_bytes: usize,
    /// The newest complete stdout lines, bounded.
    pub(super) recent: Vec<u8>,
}

/// Samples a running child's stdout every `interval` until it exits, so a
/// long invocation is observable before it finishes.
pub(super) struct ProgressReporter<'a> {
    pub(super) interval: Duration,
    pub(super) report: &'a dyn Fn(&OutputProgress),
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
    /// Called on the supervising thread between waits on the child, so only
    /// before supervision has seen it exit.
    pub(super) on_progress: Option<ProgressReporter<'a>>,
    /// Test seam for exercising wait failures without depending on another
    /// thread reaping the child between wait calls. Injected hooks are
    /// try_wait-style (non-blocking); production blocks in `wait_timeout`.
    pub(super) wait: Option<WaitHook<'a>>,
    /// Test seam: each live output reader increments this counter for the
    /// lifetime of its thread so tests can observe finalization without
    /// sampling process-wide thread counts.
    pub(super) live_readers: Option<Arc<AtomicUsize>>,
    /// Test seam for supervising an already-spawned child with its ownership
    /// guards intact. Production always spawns from the request fields.
    pub(super) spawned_child: Option<SpawnedChild>,
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
    cancel: ReaderCancel,
}

/// Supervisor-owned half of a reader's cancellation. The flag is always
/// present; the stream only wakes a reader parked in `poll` sooner.
#[cfg(unix)]
struct ReaderCancel {
    requested: Arc<AtomicBool>,
    wakeup: Option<UnixStream>,
}

#[cfg(unix)]
impl ReaderCancel {
    fn cancel(self) {
        self.requested.store(true, Ordering::Release);
        // Closing our end makes the reader's end readable (EOF).
        drop(self.wakeup);
    }
}

/// Reader-owned half of [`ReaderCancel`].
#[cfg(unix)]
struct CancelWatch {
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

/// Spawn the child [`spawn_with_timeout`] will supervise. A caller that needs
/// the child before supervision starts (the orchestrator takes the Linux
/// post-run guard off it) spawns here and passes it as `spawned_child`.
pub(super) fn spawn_for_supervision(
    program: &str,
    args: &[String],
    env: &[(String, String)],
    cwd: Option<&Path>,
    sandbox: Option<&ResolvedSandbox>,
    provider: &str,
) -> Result<SpawnedChild, SpawnError> {
    spawn_child_with_optional_sandbox(program, args, env, cwd, sandbox, provider).map_err(|err| {
        SpawnError {
            permanent: err.permanent,
            message: format!("spawn {program}: {}", err.message),
        }
    })
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
        on_progress,
        wait,
        live_readers,
        spawned_child,
        #[cfg(unix)]
        cancel_pair,
    } = request;

    let started = Instant::now();
    let spawned = match spawned_child {
        Some(spawned) => spawned,
        None => spawn_for_supervision(program, args, env, cwd, sandbox, trace.provider)?,
    };
    let SpawnedChild {
        mut child,
        // The temp profile must outlive the child — drop it after wait.
        _profile_temp,
        // Linux mount descriptors also outlive the child. Dropping a cloned
        // SQLite DB descriptor earlier can release the host's lease locks.
        _linux_mount_plan,
    } = spawned;

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

    let deadline = started + timeout;
    let sample_progress = || {
        if let Some(progress) = on_progress.as_ref()
            && let Ok(capture) = stdout_buf.lock()
        {
            let sample = OutputProgress {
                observed_bytes: capture.observed_bytes,
                recent: capture.recent(PROGRESS_WINDOW_BYTES),
            };
            drop(capture);
            (progress.report)(&sample);
        }
    };
    let wait_result = wait_until_exit_or_deadline(
        &mut child,
        deadline,
        wait,
        on_progress
            .as_ref()
            .map(|progress| (progress.interval, &sample_progress as &dyn Fn())),
    );
    // `wait` failures are host-side and not clearly deterministic — leave
    // them retryable after the common cleanup below.
    let (exit_status, wait_error, timed_out) = match wait_result {
        Ok(None) => (None, None, true),
        Ok(exit_status) => (exit_status, None, false),
        Err(err) => (None, Some(err), false),
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

/// Block until the child exits or `deadline` elapses.
///
/// Production uses `wait_timeout` for the remaining wall-clock budget so a
/// long-running agent does not wake 40 times per second. The test `wait` hook
/// is try_wait-style and may still poll.
///
/// With `progress`, production waits in slices of its interval and samples
/// between them, so sampling stops once a wait reports the exit.
fn wait_until_exit_or_deadline(
    child: &mut Child,
    deadline: Instant,
    wait: Option<WaitHook<'_>>,
    progress: Option<(Duration, &dyn Fn())>,
) -> io::Result<Option<ExitStatus>> {
    let mut next_sample = progress.map(|(interval, _)| Instant::now() + interval);
    loop {
        let slice_end = next_sample.map_or(deadline, |next| next.min(deadline));
        let remaining = slice_end.saturating_duration_since(Instant::now());
        let result = match wait {
            Some(wait) => wait(child),
            None => child.wait_timeout(remaining),
        };
        match result {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                if let (Some((interval, sample)), Some(next)) = (progress, next_sample)
                    && now >= next
                {
                    sample();
                    next_sample = Some(now + interval);
                }
                if wait.is_some() {
                    thread::sleep(Duration::from_millis(25));
                }
            }
            Err(err) => return Err(err),
        }
    }
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
    // The reader only calls `read` after `poll` reports the pipe ready, so a
    // failure here cannot turn it into a blocking reader.
    let _ = set_nonblocking(reader.as_raw_fd());
    let pair_result = cancel_pair.map_or_else(UnixStream::pair, |make_pair| make_pair());
    // Without a pair the reader falls back to timed polls of the cancel flag.
    let (wakeup, cancel_wakeup) = match pair_result {
        Ok((wakeup, cancel)) => {
            let _ = wakeup.set_nonblocking(true);
            let _ = cancel.set_nonblocking(true);
            (Some(wakeup), Some(cancel))
        }
        Err(_) => (None, None),
    };
    let requested = Arc::new(AtomicBool::new(false));
    let watch = CancelWatch {
        requested: Arc::clone(&requested),
        wakeup,
    };

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
        cancel: ReaderCancel {
            requested,
            wakeup: cancel_wakeup,
        },
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
        match poll_reader_or_cancel(reader.as_raw_fd(), watch) {
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
    if line_buf.is_empty() || !tracing::enabled!(tracing::Level::INFO) {
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
enum PollOutcome {
    Readable,
    Cancelled,
    Failed,
}

#[cfg(unix)]
fn poll_reader_or_cancel(reader_fd: RawFd, watch: &CancelWatch) -> PollOutcome {
    let mut fds = [
        libc::pollfd {
            fd: reader_fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: watch.wakeup.as_ref().map_or(-1, AsRawFd::as_raw_fd),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // With a wakeup fd the reader can park indefinitely; without one it must
    // wake periodically to observe the cancel flag.
    let (nfds, timeout_ms) = if watch.wakeup.is_some() {
        (2, -1)
    } else {
        (1, CANCEL_FLAG_POLL_INTERVAL.as_millis() as libc::c_int)
    };
    loop {
        // The flag is checked on every pass so a continuously readable pipe
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
        let child_id = child.id();
        let _ = signal_child_process_group(child_id, libc::SIGKILL);
        let _ = child.kill();
        let _ = child.wait();
        wait_for_process_group_exit(child_id);
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(unix)]
fn wait_for_process_group_exit(child_id: u32) {
    let deadline = Instant::now() + PROCESS_GROUP_CLEANUP_TIMEOUT;
    while process_group_is_alive(child_id) {
        let _ = signal_child_process_group(child_id, libc::SIGKILL);
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
fn process_group_is_alive(child_id: u32) -> bool {
    if child_id == 0 || child_id > i32::MAX as u32 {
        return false;
    }
    let rc = unsafe { libc::killpg(child_id as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
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
    if !tracing::enabled!(tracing::Level::INFO) {
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
