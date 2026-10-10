use std::io;
use std::path::Path;
use std::process::{Child, ChildStdin, ExitStatus};
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::fs::File;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};

use orbit_common::process::build_budget::{WAIT_DIRECTORY_ENV, read_waits};
use orbit_common::process::stopped_descendants::{StoppedDescendant, StoppedDescendantWatch};
use wait_timeout::ChildExt;

use super::super::super::dispatcher::ResolvedSandbox;
use super::super::spawn::{SpawnError, SpawnedChild, spawn_child_with_optional_sandbox};
use super::capture::{
    CapturedOutput, OutputProgress, RollingOutputCapture, default_output_capture_limit,
    finish_captured_output,
};
#[cfg(unix)]
use super::output::{
    CancelPairHook, PipeCancel, PollOutcome, pipe_cancellation, poll_pipe_or_cancel,
    set_nonblocking,
};
use super::output::{
    OUTPUT_READER_JOIN_TIMEOUT, OutputReaderContext, join_output_reader, spawn_output_reader,
};

type SpawnOutput = (CapturedOutput, CapturedOutput, Option<i32>, Duration, bool);

const PROCESS_GROUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(1);

/// Newest stdout bytes a progress sample carries. Large enough for one
/// provider event holding a long assistant message.
const PROGRESS_WINDOW_BYTES: usize = 64 * 1024;

type WaitHook<'a> = &'a dyn Fn(&mut Child) -> std::io::Result<Option<ExitStatus>>;

/// Samples a running child's stdout every `interval` until it exits, so a
/// long invocation is observable before it finishes.
pub(in crate::activity_job::cli_runner) struct ProgressReporter<'a> {
    pub(in crate::activity_job::cli_runner) interval: Duration,
    pub(in crate::activity_job::cli_runner) report: &'a dyn Fn(&OutputProgress),
}

/// Ends descendants of the child that stay stopped past `threshold`, and
/// reports each one it ended or failed to end.
pub(in crate::activity_job::cli_runner) struct StoppedDescendantReporter<'a> {
    pub(in crate::activity_job::cli_runner) threshold: Duration,
    pub(in crate::activity_job::cli_runner) report: &'a dyn Fn(&StoppedDescendant),
}

pub(in crate::activity_job::cli_runner) struct SpawnTraceContext<'a> {
    pub(in crate::activity_job::cli_runner) provider: &'a str,
    pub(in crate::activity_job::cli_runner) job_run_id: &'a str,
    pub(in crate::activity_job::cli_runner) task_id: Option<&'a str>,
    pub(in crate::activity_job::cli_runner) cwd: Option<&'a str>,
}

pub(in crate::activity_job::cli_runner) struct SpawnWithTimeoutRequest<'a> {
    pub(in crate::activity_job::cli_runner) program: &'a str,
    pub(in crate::activity_job::cli_runner) args: &'a [String],
    pub(in crate::activity_job::cli_runner) stdin_bytes: &'a [u8],
    pub(in crate::activity_job::cli_runner) env: &'a [(String, String)],
    pub(in crate::activity_job::cli_runner) cwd: Option<&'a Path>,
    pub(in crate::activity_job::cli_runner) timeout: Duration,
    pub(in crate::activity_job::cli_runner) sandbox: Option<&'a ResolvedSandbox>,
    pub(in crate::activity_job::cli_runner) trace: SpawnTraceContext<'a>,
    pub(in crate::activity_job::cli_runner) output_capture_limit: Option<usize>,
    /// [ORB-10496] Invoked once with the spawned child's PID, immediately after
    /// spawn and before the supervision loop. The PID is otherwise visible only
    /// inside this module (process-group cleanup), so a long-running provider
    /// child has no observable identity while it runs.
    pub(in crate::activity_job::cli_runner) on_spawn: Option<&'a dyn Fn(u32)>,
    /// Called on the supervising thread between waits on the child, so only
    /// before supervision has seen it exit.
    pub(in crate::activity_job::cli_runner) on_progress: Option<ProgressReporter<'a>>,
    /// Without it, a descendant that stops itself holds the child until the
    /// deadline. Checked between waits on the child, like `on_progress`.
    pub(in crate::activity_job::cli_runner) stopped_descendants:
        Option<StoppedDescendantReporter<'a>>,
    /// Test seam for exercising wait failures without depending on another
    /// thread reaping the child between wait calls. Injected hooks are
    /// try_wait-style (non-blocking); production blocks in `wait_timeout`.
    pub(in crate::activity_job::cli_runner) wait: Option<WaitHook<'a>>,
    /// Test seam: each live output reader increments this counter for the
    /// lifetime of its thread so tests can observe finalization without
    /// sampling process-wide thread counts.
    pub(in crate::activity_job::cli_runner) live_readers: Option<Arc<AtomicUsize>>,
    /// Test seam for supervising an already-spawned child with its ownership
    /// guards intact. Production always spawns from the request fields.
    pub(in crate::activity_job::cli_runner) spawned_child: Option<SpawnedChild>,
    /// Test seam for exercising pipe workers when the pollable cancellation
    /// channel cannot be constructed.
    #[cfg(unix)]
    pub(in crate::activity_job::cli_runner) cancel_pair: Option<CancelPairHook<'a>>,
}

struct StdinWriterHandle {
    join: thread::JoinHandle<()>,
    #[cfg(unix)]
    cancel: PipeCancel,
}

impl StdinWriterHandle {
    fn cancel(self) -> thread::JoinHandle<()> {
        #[cfg(unix)]
        self.cancel.cancel();
        self.join
    }
}

/// Spawn the child [`spawn_with_timeout`] will supervise. A caller that needs
/// the child before supervision starts (the orchestrator takes the Linux
/// post-run guard off it) spawns here and passes it as `spawned_child`.
pub(in crate::activity_job::cli_runner) fn spawn_for_supervision(
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

pub(in crate::activity_job::cli_runner) fn spawn_with_timeout(
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
        stopped_descendants,
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

    let stdin_writer = match child
        .stdin
        .take()
        .map(|stdin| {
            spawn_stdin_writer(
                stdin,
                stdin_bytes,
                #[cfg(unix)]
                cancel_pair,
            )
        })
        .transpose()
    {
        Ok(writer) => writer,
        Err(err) => {
            kill_child_process_tree(&mut child);
            return Err(SpawnError {
                permanent: err.kind() == io::ErrorKind::Unsupported,
                message: format!("stdin {program}: {err}"),
            });
        }
    };

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
    let wait_directory = env
        .iter()
        .rev()
        .find(|(name, _)| name == WAIT_DIRECTORY_ENV)
        .map(|(_, value)| Path::new(value));
    let extended_deadline = || {
        let credit = wait_directory.map_or(0, |directory| {
            read_waits(
                directory,
                timeout.as_millis().try_into().unwrap_or(u64::MAX),
            )
            .deadline_extension_ms
        });
        deadline + Duration::from_millis(credit)
    };
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
    let mut stopped_watch = stopped_descendants.map(|reporter| StoppedWatch {
        watch: StoppedDescendantWatch::new(reporter.threshold),
        report: reporter.report,
    });
    let wait_result = wait_until_exit_or_deadline(
        &mut child,
        deadline,
        wait,
        on_progress
            .as_ref()
            .map(|progress| (progress.interval, &sample_progress as &dyn Fn())),
        stopped_watch.as_mut(),
        wait_directory.map(|_| &extended_deadline as &dyn Fn() -> Instant),
    );
    // `wait` failures are host-side and not clearly deterministic — leave
    // them retryable after the common cleanup below.
    let (exit_status, wait_error, timed_out) = match wait_result {
        Ok(None) => (None, None, true),
        Ok(exit_status) => (exit_status, None, false),
        Err(err) => (None, Some(err), false),
    };

    let stdin_join = stdin_writer.map(StdinWriterHandle::cancel);
    kill_child_process_tree(&mut child);

    // The join is bounded on every exit path, not only after a timeout. A
    // reader returns when the last writer closes the pipe, and a helper the
    // agent left in its own session (`setsid`) keeps the write end open after
    // the child itself exits and after the group kill above. An unbounded
    // join there never returns, no finish event is emitted, and the run's
    // reservation is never released.
    let reader_join_deadline = Instant::now() + OUTPUT_READER_JOIN_TIMEOUT;
    if let Some(join) = stdin_join {
        // The nonblocking writer observes cancellation within one poll
        // interval, fitting inside the readers' existing shared window.
        // Join before returning to release its prompt and owned descriptors.
        let _ = join.join();
    }
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

struct StoppedWatch<'a> {
    watch: StoppedDescendantWatch,
    report: &'a dyn Fn(&StoppedDescendant),
}

/// Block until the child exits or `deadline` elapses.
///
/// Production uses `wait_timeout` for the remaining wall-clock budget so a
/// long-running agent does not wake 40 times per second. The test `wait` hook
/// is try_wait-style and may still poll.
///
/// With `progress`, production waits in slices of its interval and samples
/// between them, so sampling stops once a wait reports the exit. `stopped`
/// slices the wait the same way, and samples the child's process tree only
/// while the child is unreaped.
fn wait_until_exit_or_deadline(
    child: &mut Child,
    deadline: Instant,
    wait: Option<WaitHook<'_>>,
    progress: Option<(Duration, &dyn Fn())>,
    mut stopped: Option<&mut StoppedWatch<'_>>,
    extended_deadline: Option<&dyn Fn() -> Instant>,
) -> io::Result<Option<ExitStatus>> {
    let mut next_sample = progress.map(|(interval, _)| Instant::now() + interval);
    loop {
        let deadline = extended_deadline.map_or(deadline, |read| read());
        let mut slice_end = next_sample.map_or(deadline, |next| next.min(deadline));
        if extended_deadline.is_some() {
            slice_end = slice_end.min(Instant::now() + Duration::from_millis(100));
        }
        if let Some(stopped) = stopped.as_ref() {
            slice_end = slice_end.min(stopped.watch.next_sample_at());
        }
        let remaining = slice_end.saturating_duration_since(Instant::now());
        let result = match wait {
            Some(wait) => wait(child),
            None => child.wait_timeout(remaining),
        };
        match result {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {
                let now = Instant::now();
                if now >= extended_deadline.map_or(deadline, |read| read()) {
                    return Ok(None);
                }
                if let (Some((interval, sample)), Some(next)) = (progress, next_sample)
                    && now >= next
                {
                    sample();
                    next_sample = Some(now + interval);
                }
                if let Some(stopped) = stopped.as_deref_mut() {
                    for descendant in stopped.watch.poll(child.id()) {
                        (stopped.report)(&descendant);
                    }
                }
                if wait.is_some() {
                    thread::sleep(Duration::from_millis(25));
                }
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(unix)]
fn spawn_stdin_writer(
    stdin: ChildStdin,
    bytes: &[u8],
    cancel_pair: Option<CancelPairHook<'_>>,
) -> io::Result<StdinWriterHandle> {
    let fd = stdin.into_raw_fd();
    // SAFETY: `into_raw_fd` transferred sole ownership of the stdin pipe.
    let mut writer = unsafe { File::from_raw_fd(fd) };
    // Unlike a ready read, a large write can block even after POLLOUT.
    // Nonblocking mode is mandatory before starting the worker.
    set_nonblocking(writer.as_raw_fd())?;
    let (cancel, watch) = pipe_cancellation(cancel_pair);
    let bytes = bytes.to_vec();
    let join = thread::Builder::new().spawn(move || {
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            match poll_pipe_or_cancel(writer.as_raw_fd(), libc::POLLOUT, &watch) {
                PollOutcome::Ready => match writer.write(remaining) {
                    Ok(0) => break,
                    Ok(n) => remaining = &remaining[n..],
                    Err(err)
                        if matches!(
                            err.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                        ) =>
                    {
                        continue;
                    }
                    Err(_) => break,
                },
                PollOutcome::Cancelled | PollOutcome::Failed => break,
            }
        }
    })?;
    Ok(StdinWriterHandle { join, cancel })
}

#[cfg(not(unix))]
fn spawn_stdin_writer(_stdin: ChildStdin, _bytes: &[u8]) -> io::Result<StdinWriterHandle> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "bounded provider stdin supervision requires Unix (use WSL2 on Windows)",
    ))
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
