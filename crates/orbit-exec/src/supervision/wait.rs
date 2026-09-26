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
use super::tee::{output_capture_limit, spawn_stderr_drain, spawn_stdin_write, spawn_stdout_drain};

pub(crate) const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

type StdinResultReceiver = Receiver<std::io::Result<()>>;
type StdinWorker = (Option<StdinResultReceiver>, Option<JoinHandle<()>>);

/// Output collected from a spawned process.
pub(crate) struct WaitResult {
    pub(crate) exit_success: bool,
    pub(crate) exit_code: Option<i32>,
    pub(crate) stdout: Vec<u8>,
    /// Stderr text; includes "process timed out" appended when timed out.
    pub(crate) stderr: Vec<u8>,
    /// Whether the wall-clock deadline elapsed and the supervisor terminated
    /// the child's process group. Callers that must distinguish a timeout from
    /// an ordinary nonzero exit read this instead of matching stderr text.
    pub(crate) timed_out: bool,
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
    mut child: Child,
    timeout_ms: Option<u64>,
    debug: bool,
    stdin_payload: Option<Vec<u8>>,
    output_limit: usize,
) -> Result<WaitResult, OrbitError> {
    // Drain stdout/stderr in background threads so the child never blocks on a
    // full pipe buffer (which would prevent it from exiting).
    //
    // In debug mode, both stdout and stderr are tee'd through redaction-aware
    // drains so the user sees live output without bypassing capture/redaction.
    let (stdin_result_rx, stdin_thread) = spawn_stdin_thread(&mut child, stdin_payload)?;
    // Each of the two drain threads reports its capture limit at most once.
    let (output_limit_tx, output_limit_rx) = mpsc::sync_channel(2);
    let stdout_thread = child
        .stdout
        .take()
        .map(|out| spawn_stdout_drain(out, debug, output_limit, output_limit_tx.clone()));
    let stderr_thread = child
        .stderr
        .take()
        .map(|err| spawn_stderr_drain(err, debug, output_limit, output_limit_tx));

    // Last drop restores the previous SIGINT/SIGTERM disposition and
    // re-raises a captured signal so daemons still shut down.
    #[cfg(unix)]
    let mut signal_guard = SignalHandlerGuard::install(child.id())?;

    let deadline = timeout_ms.map(|ms| Instant::now() + Duration::from_millis(ms));
    let mut stdin_write_error = None;
    let mut capture_limited: Option<&'static str> = None;
    let (timed_out, interrupted_signal, exit_success, exit_code) = loop {
        if let Ok(stream) = output_limit_rx.try_recv() {
            terminate_process_group(&mut child, termination_signal(), WAIT_POLL_INTERVAL)?;
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
                    terminate_process_group(&mut child, termination_signal(), WAIT_POLL_INTERVAL)?;
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

        if let Some(status) = child
            .wait_timeout(wait_slice)
            .map_err(|e| OrbitError::Execution(format!("wait timeout error: {e}")))?
        {
            // The child is reaped: its pid is free for reuse from here on, so
            // a SIGINT/SIGTERM arriving before this wait returns must not
            // `killpg` whatever process group now owns that number.
            #[cfg(unix)]
            signal_guard.release_process_group();

            #[cfg(unix)]
            if let Some(signal) = signal_guard.take_signal() {
                terminate_orphaned_process_group(child.id(), signal, WAIT_POLL_INTERVAL);
                break (false, Some(signal), false, Some(128 + signal));
            }

            // Child exited successfully within the timeout. Kill its process
            // group so any orphan subprocesses still holding the pipes open
            // are reaped before we join the reader threads below.
            kill_process_group(child.id());
            break (false, None, status.success(), status.code());
        }

        #[cfg(unix)]
        if let Some(signal) = signal_guard.take_signal() {
            terminate_process_group(&mut child, signal, WAIT_POLL_INTERVAL)?;
            break (false, Some(signal), false, Some(128 + signal));
        }

        if deadline.is_some_and(|end| Instant::now() >= end) {
            terminate_process_group(&mut child, termination_signal(), WAIT_POLL_INTERVAL)?;
            break (true, None, false, None);
        }
    };
    // Every exit above has reaped the child (directly or through
    // `terminate_process_group`); stop fanning signals out to its old group
    // before the reader joins below, which can outlast a pid's reuse.
    #[cfg(unix)]
    signal_guard.release_process_group();

    // Join reader threads. They complete quickly once the process group is
    // killed (all pipe write ends are closed -> EOF).
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
        // real exit status; treat it the same as the in-loop EPIPE case and
        // let the already-captured exit status and stderr tail stand.
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
    if let Some(stream) = capture_limited {
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

    Ok(WaitResult {
        exit_success,
        exit_code,
        stdout,
        stderr,
        timed_out,
    })
}

fn spawn_stdin_thread(
    child: &mut Child,
    stdin_payload: Option<Vec<u8>>,
) -> Result<StdinWorker, OrbitError> {
    match stdin_payload {
        Some(bytes) => {
            let stdin = child.stdin.take().ok_or_else(|| {
                OrbitError::Execution("stdin requested but no stdin pipe available".to_string())
            })?;
            // The single stdin writer sends one completion result.
            let (tx, rx) = mpsc::sync_channel(1);
            let handle = spawn_stdin_write(stdin, bytes, tx);
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
