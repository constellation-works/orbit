use std::io::PipeWriter;
use std::process::Child;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use wait_timeout::ChildExt;

#[cfg(unix)]
use super::cleanup::terminate_orphaned_process_group;
use super::cleanup::{kill_process_group, terminate_process_group, termination_signal};
#[cfg(unix)]
use super::signal::{SignalHandlerGuard, signal_message};
use super::tee::{
    DRAIN_BUDGET, DrainStop, StopWatch, output_capture_limit, spawn_relay_drain,
    spawn_stderr_drain, spawn_stdin_write, spawn_stdout_drain,
};

pub(crate) const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

type StdinResultReceiver = Receiver<std::io::Result<()>>;
type StdinWorker = (Option<StdinResultReceiver>, Option<JoinHandle<()>>);

/// Own cleanup from the first setup operation until the child is reaped.
/// `Child` itself does neither kill nor wait on drop.
pub(crate) struct SupervisedChild {
    process: Child,
    reaped: bool,
    #[cfg(unix)]
    signal_guard: Option<SignalHandlerGuard>,
}

impl SupervisedChild {
    pub(crate) fn new(process: Child) -> Self {
        Self {
            process,
            reaped: false,
            #[cfg(unix)]
            signal_guard: None,
        }
    }

    pub(crate) fn process_mut(&mut self) -> &mut Child {
        &mut self.process
    }

    fn mark_reaped(&mut self) {
        self.reaped = true;
        #[cfg(unix)]
        if let Some(guard) = self.signal_guard.as_mut() {
            guard.release_process_group();
        }
    }

    #[cfg(unix)]
    fn take_signal(&self) -> Option<i32> {
        self.signal_guard
            .as_ref()
            .and_then(SignalHandlerGuard::take_signal)
    }
}

impl Drop for SupervisedChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        kill_process_group(self.process.id());
        let _ = self.process.kill();
        let _ = self.process.wait();
        // Release the slot before dropping the signal guard: its last drop
        // can re-raise a pending signal and terminate the supervisor itself.
        self.mark_reaped();
    }
}

// Per-thread fault injection leaves concurrent supervisors unaffected and
// exercises the same entry points callers use, without exhausting host FDs.
#[cfg(all(test, unix))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SupervisionFailure {
    DrainStop,
    Watch(usize),
    SignalInstall,
    Wait,
}

#[cfg(all(test, unix))]
thread_local! {
    static FAILURE: std::cell::Cell<Option<SupervisionFailure>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(all(test, unix))]
pub(super) fn with_supervision_failure<T>(failure: SupervisionFailure, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<SupervisionFailure>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAILURE.set(self.0);
        }
    }
    let _restore = Restore(FAILURE.replace(Some(failure)));
    let result = f();
    assert!(
        FAILURE.get().is_none(),
        "supervisor did not reach {failure:?}"
    );
    result
}

#[cfg(all(test, unix))]
fn inject_supervision_failure(point: SupervisionFailure) -> Result<(), OrbitError> {
    if FAILURE.get() == Some(point) {
        FAILURE.set(None);
        return Err(OrbitError::Execution(format!(
            "injected supervision failure: {point:?}"
        )));
    }
    Ok(())
}

/// Output collected from a spawned process.
pub(crate) struct WaitResult {
    pub(crate) exit_success: bool,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    /// Stderr text, with supervision diagnostics appended.
    pub(crate) stderr: Vec<u8>,
    /// Whether the wall-clock deadline elapsed and the supervisor terminated
    /// the child's process group. Callers that must distinguish a timeout from
    /// an ordinary nonzero exit read this instead of matching stderr text.
    pub(crate) timed_out: bool,
    /// Whether a pipe was still held open [`DRAIN_BUDGET`] after the child
    /// was reaped — by a descendant outside its process group — so the pipe
    /// workers were stopped instead of reaching EOF. Callers see this as a
    /// stderr note; the supervision tests read the flag.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) drain_stopped: bool,
}

pub(crate) fn wait_with_optional_timeout(
    child: Child,
    timeout_ms: Option<u64>,
    debug: bool,
    stdin_payload: Option<Vec<u8>>,
) -> Result<WaitResult, OrbitError> {
    wait_with_timeout_and_output_limit(
        child,
        timeout_ms,
        debug,
        stdin_payload,
        output_capture_limit(),
    )
}

pub(super) fn wait_with_timeout_and_output_limit(
    child: Child,
    timeout_ms: Option<u64>,
    debug: bool,
    stdin_payload: Option<Vec<u8>>,
    output_limit: usize,
) -> Result<WaitResult, OrbitError> {
    wait_cancellable(
        SupervisedChild::new(child),
        timeout_ms,
        debug,
        stdin_payload,
        output_limit,
        None,
        None,
    )
}

/// Supervise `child` like [`wait_with_optional_timeout`], forwarding its
/// stdout into `relay` instead of capturing it. The relay is closed within
/// the same drain bound, so its reader always reaches EOF.
pub(crate) fn wait_with_stdout_relay(
    child: SupervisedChild,
    timeout_ms: Option<u64>,
    debug: bool,
    stdin_payload: Option<Vec<u8>>,
    relay: Option<PipeWriter>,
) -> Result<WaitResult, OrbitError> {
    wait_cancellable(
        child,
        timeout_ms,
        debug,
        stdin_payload,
        output_capture_limit(),
        relay,
        None,
    )
}

pub(crate) fn wait_with_cancellation(
    child: Child,
    timeout_ms: Option<u64>,
    stdin_payload: Option<Vec<u8>>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<WaitResult, OrbitError> {
    wait_cancellable(
        SupervisedChild::new(child),
        timeout_ms,
        false,
        stdin_payload,
        output_capture_limit(),
        None,
        cancelled,
    )
}

fn wait_cancellable(
    mut child: SupervisedChild,
    timeout_ms: Option<u64>,
    debug: bool,
    stdin_payload: Option<Vec<u8>>,
    output_limit: usize,
    stdout_relay: Option<PipeWriter>,
    cancelled: Option<&std::sync::atomic::AtomicBool>,
) -> Result<WaitResult, OrbitError> {
    // Every pipe worker is bounded by `drain_stop`: once the child is gone
    // the supervisor waits at most `DRAIN_BUDGET` for them (see its docs).
    // Dropping it on an early error return stops them as well.
    #[cfg(all(test, unix))]
    inject_supervision_failure(SupervisionFailure::DrainStop)?;
    let drain_stop = DrainStop::new().map_err(|err| {
        OrbitError::Execution(format!("failed to set up process pipe supervision: {err}"))
    })?;
    #[cfg(all(test, unix))]
    let watch_count = std::cell::Cell::new(0);
    let watch = || {
        #[cfg(all(test, unix))]
        {
            let count = watch_count.get();
            watch_count.set(count + 1);
            inject_supervision_failure(SupervisionFailure::Watch(count))?;
        }
        drain_stop.watch().map_err(|err| {
            OrbitError::Execution(format!("failed to set up process pipe supervision: {err}"))
        })
    };

    // Drain stdout/stderr in background threads so the child never blocks on a
    // full pipe buffer (which would prevent it from exiting).
    //
    // In debug mode, both stdout and stderr are tee'd through redaction-aware
    // drains so the user sees live output without bypassing capture/redaction.
    let (stdin_result_rx, stdin_thread) =
        spawn_stdin_thread(&mut child.process, stdin_payload, watch)?;
    // Each of the two drain threads reports its capture limit at most once.
    let (output_limit_tx, output_limit_rx) = mpsc::sync_channel(2);
    let stdout_thread = match (child.process.stdout.take(), stdout_relay) {
        (Some(out), Some(relay)) => Some(spawn_relay_drain(out, relay, watch()?)),
        (Some(out), None) => Some(spawn_stdout_drain(
            out,
            debug,
            output_limit,
            output_limit_tx.clone(),
            watch()?,
        )),
        (None, _) => None,
    };
    let stderr_thread = match child.process.stderr.take() {
        Some(err) => Some(spawn_stderr_drain(
            err,
            debug,
            output_limit,
            output_limit_tx,
            watch()?,
        )),
        None => None,
    };

    // Keep the handler in the child guard so error cleanup kills and reaps
    // before its last drop restores handlers and re-raises a pending signal.
    #[cfg(all(test, unix))]
    inject_supervision_failure(SupervisionFailure::SignalInstall)?;
    #[cfg(unix)]
    {
        child.signal_guard = Some(SignalHandlerGuard::install(child.process.id())?);
    }

    let deadline = timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
    let mut stdin_write_error = None;
    let mut capture_limited: Option<&'static str> = None;
    // Annotated because the only `Some(signal)` arms are Unix-only.
    let (timed_out, interrupted_signal, mut exit_success, exit_code): (
        bool,
        Option<i32>,
        bool,
        Option<i32>,
    ) = loop {
        if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
            kill_process_group(child.process.id());
            let _ = child.process.kill();
            let _ = child.process.wait();
            break (false, None, false, None);
        }
        if let Ok(stream) = output_limit_rx.try_recv() {
            terminate_process_group(&mut child.process, termination_signal(), WAIT_POLL_INTERVAL)?;
            capture_limited = Some(stream);
            break (false, None, false, None);
        }

        if let Some(rx) = stdin_result_rx.as_ref() {
            match rx.try_recv() {
                Ok(Ok(())) => {}
                // A backend that exits before consuming the request envelope
                // (missing interpreter, empty shim, launcher error) closes its
                // stdin pipe; the writer then observes EPIPE. That is not a
                // supervisor-side failure, so fall through instead of
                // terminating: the wait loop below reaps the child's real
                // exit status and stderr tail, matching the non-zero-exit
                // diagnostic instead of a bare "Broken pipe" error.
                Ok(Err(err)) if err.kind() == std::io::ErrorKind::BrokenPipe => {}
                Ok(Err(err)) => {
                    terminate_process_group(
                        &mut child.process,
                        termination_signal(),
                        WAIT_POLL_INTERVAL,
                    )?;
                    stdin_write_error = Some(OrbitError::Execution(format!(
                        "failed to write process stdin: {err}"
                    )));
                    break (false, None, false, None);
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {}
            }
        }

        let wait_slice = deadline
            .map(|end| {
                end.saturating_duration_since(Instant::now())
                    .min(WAIT_POLL_INTERVAL)
            })
            .unwrap_or(WAIT_POLL_INTERVAL);

        #[cfg(all(test, unix))]
        inject_supervision_failure(SupervisionFailure::Wait)?;
        if let Some(status) = child
            .process
            .wait_timeout(wait_slice)
            .map_err(|e| OrbitError::Execution(format!("wait timeout error: {e}")))?
        {
            // The child is reaped: its pid is free for reuse from here on, so
            // a SIGINT/SIGTERM arriving before this wait returns must not
            // `killpg` whatever process group now owns that number.
            child.mark_reaped();

            #[cfg(unix)]
            if let Some(signal) = child.take_signal() {
                terminate_orphaned_process_group(child.process.id(), signal, WAIT_POLL_INTERVAL);
                break (false, Some(signal), false, Some(128 + signal));
            }

            // Child exited successfully within the timeout. Kill its process
            // group so any orphan subprocesses still holding the pipes open
            // are reaped before the pipe workers settle below.
            kill_process_group(child.process.id());
            break (false, None, status.success(), status.code());
        }

        #[cfg(unix)]
        if let Some(signal) = child.take_signal() {
            terminate_process_group(&mut child.process, signal, WAIT_POLL_INTERVAL)?;
            break (false, Some(signal), false, Some(128 + signal));
        }

        if deadline.is_some_and(|end| Instant::now() >= end) {
            terminate_process_group(&mut child.process, termination_signal(), WAIT_POLL_INTERVAL)?;
            break (true, None, false, None);
        }
    };
    // Every exit above has reaped the child (directly or through
    // `terminate_process_group`); stop fanning signals out to its old group
    // before the pipe workers settle below, which can outlast a pid's reuse.
    child.mark_reaped();

    // The process group is dead, so its pipe ends are closed and the workers
    // normally hit EOF at once. Only a holder outside the group keeps one
    // open; `settle` stops such workers after the budget, so the joins below
    // are bounded either way.
    let drain_stopped = drain_stop.settle(DRAIN_BUDGET);
    let stdout = stdout_thread
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let mut stderr = stderr_thread
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stdin_thread_panicked = stdin_thread.map(|h| h.join().is_err()).unwrap_or(false);

    if stdin_thread_panicked {
        return Err(OrbitError::Execution(
            "stdin writer thread panicked".to_string(),
        ));
    }
    if let Some(error) = stdin_write_error {
        return Err(error);
    }
    if !timed_out
        && interrupted_signal.is_none()
        && let Some(Err(err)) = receive_stdin_result(stdin_result_rx)
        && err.kind() != std::io::ErrorKind::BrokenPipe
    {
        // A BrokenPipe here means the write raced the child's own exit and
        // observed EPIPE only after `wait_timeout` above already reaped the
        // real exit status, or that the writer was stopped because only a
        // holder outside the process group still had the pipe. Treat it the
        // same as the in-loop EPIPE case and let the already-captured exit
        // status and stderr tail stand.
        return Err(OrbitError::Execution(format!(
            "failed to write process stdin: {err}"
        )));
    }

    if timed_out {
        if !stderr.is_empty() {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(b"process timed out");
    }
    // A capture-limit stop looks like a bare failure otherwise (exit code
    // `None`, often an empty stderr), and a tool such as `github.run.logs`
    // then reports "failed: " with no reason for a log that was merely long.
    // The workers have joined, so also drain notifications sent while the
    // child was exiting or the pipes were settling. Truncated output must
    // fail even if the child itself exited successfully.
    for stream in capture_limited
        .into_iter()
        .chain(output_limit_rx.try_iter())
    {
        exit_success = false;
        if !stderr.is_empty() {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(
            format!("process output capture limit exceeded on {stream}").as_bytes(),
        );
    }
    #[cfg(unix)]
    if !timed_out && let Some(signal) = interrupted_signal {
        if !stderr.is_empty() {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(signal_message(signal).as_bytes());
    }

    #[cfg(not(unix))]
    let _ = interrupted_signal;
    // Output after this point was discarded; without the note a caller could
    // mistake a cut stream for the whole of it.
    if drain_stopped {
        if !stderr.is_empty() {
            stderr.push(b'\n');
        }
        stderr.extend_from_slice(
            format!(
                "process pipes were still held open outside its process group; \
                 stopped draining {} ms after it ended",
                DRAIN_BUDGET.as_millis()
            )
            .as_bytes(),
        );
    }

    Ok(WaitResult {
        exit_success,
        exit_code,
        stdout,
        stderr,
        timed_out,
        drain_stopped,
    })
}

fn spawn_stdin_thread(
    child: &mut Child,
    stdin_payload: Option<Vec<u8>>,
    watch: impl FnOnce() -> Result<StopWatch, OrbitError>,
) -> Result<StdinWorker, OrbitError> {
    match stdin_payload {
        Some(bytes) => {
            let stdin = child.stdin.take().ok_or_else(|| {
                OrbitError::Execution("stdin requested but no stdin pipe available".to_string())
            })?;
            // The single stdin writer sends one completion result.
            let (tx, rx) = mpsc::sync_channel(1);
            let handle = spawn_stdin_write(stdin, bytes, tx, watch()?);
            Ok((Some(rx), Some(handle)))
        }
        None => Ok((None, None)),
    }
}

fn receive_stdin_result(
    stdin_result_rx: Option<StdinResultReceiver>,
) -> Option<std::io::Result<()>> {
    stdin_result_rx.and_then(|rx| rx.recv().ok())
}
