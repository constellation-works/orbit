//! The supervisor's wait loop.

use std::process::Child;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

use super::super::OwnedChild;
use super::super::stopped_descendants::{StoppedDescendant, StoppedDescendantWatch};
use super::drain::{DrainStop, StopWatch, spawn_sink_drain, spawn_stdin_write};
#[cfg(unix)]
use super::signal_guard::SignalHandlerGuard;
use super::{Outcome, ProcessStopCause, Stdin, Supervision, Termination};
use crate::OrbitError;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);

type StdinResultReceiver = Receiver<std::io::Result<()>>;
type StdinWorker = (Option<StdinResultReceiver>, Option<JoinHandle<()>>);

/// A child under supervision, owned from the first setup operation until it
/// is reaped. Dropping it unsupervised kills and reaps its process group.
pub struct SupervisedChild {
    process: OwnedChild,
    #[cfg(unix)]
    signal_guard: Option<SignalHandlerGuard>,
}

impl SupervisedChild {
    /// Intercept termination signals, then run `spawn`.
    ///
    /// A signal arriving while `spawn` runs — including any post-spawn work it
    /// does, such as binding the child's pid — stays pending until supervision
    /// ends the child's group, so it cannot take the previous disposition and
    /// orphan the group. The interception lasts until the child is reaped,
    /// including errors during later pipe setup.
    ///
    /// # Errors
    /// Returns the handler installation error or `spawn`'s error.
    pub fn spawn_guarded<C: Into<OwnedChild>>(
        spawn: impl FnOnce() -> Result<C, OrbitError>,
    ) -> Result<Self, OrbitError> {
        #[cfg(unix)]
        let signal_guard = SignalHandlerGuard::install()?;
        let child = Self::adopt(spawn()?);
        #[cfg(unix)]
        let mut child = child;
        #[cfg(unix)]
        {
            child.signal_guard = Some(signal_guard);
            child.register_process_group();
        }
        Ok(child)
    }

    /// Take ownership of a child spawned as its own process-group leader.
    /// The caller owns the interval between its spawn and this call.
    pub fn adopt(process: impl Into<OwnedChild>) -> Self {
        Self {
            process: process.into(),
            #[cfg(unix)]
            signal_guard: None,
        }
    }

    /// Borrow the child, for pipe setup before supervision.
    pub fn child_mut(&mut self) -> &mut Child {
        self.process.child_mut()
    }

    fn mark_reaped(&mut self) {
        self.process.mark_reaped();
        self.release_process_group();
    }

    fn release_process_group(&mut self) {
        #[cfg(unix)]
        if let Some(guard) = self.signal_guard.as_mut() {
            guard.release_process_group();
        }
    }

    #[cfg(unix)]
    fn register_process_group(&mut self) {
        if let Some(guard) = self.signal_guard.as_mut() {
            guard.register_process_group(self.process.pid());
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
        self.process.kill_and_reap();
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

impl SupervisedChild {
    /// Supervise the child until it exits or the supervisor stops it, drain
    /// its pipes within the drain budget, and report what happened.
    ///
    /// # Errors
    /// Returns pipe, handler or wait setup failures and stdin write failures
    /// other than a broken pipe. The child's group is killed and the child
    /// reaped before any error returns.
    pub fn supervise(mut self, run: Supervision<'_>) -> Result<Outcome, OrbitError> {
        let Supervision {
            stdin,
            stdout,
            stderr,
            timeout,
            termination,
            cancelled,
            intercept_signals,
            drain_budget,
            stopped_descendants: stopped_threshold,
        } = run;
        let child = &mut self;

        // Guarded spawns already intercept signals. Adopt externally spawned
        // children before any fallible pipe setup, and keep cleanup ownership
        // if handler installation fails.
        #[cfg(unix)]
        if intercept_signals && child.signal_guard.is_none() {
            #[cfg(test)]
            inject_supervision_failure(SupervisionFailure::SignalInstall)?;
            child.signal_guard = Some(SignalHandlerGuard::install()?);
            child.register_process_group();
        }
        #[cfg(not(unix))]
        let _ = intercept_signals;

        // Every pipe worker is bounded by `drain_stop`: once the child is gone
        // the supervisor waits at most `drain_budget` for them. Dropping it on
        // an early error return stops them as well.
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

        // Drain stdout/stderr in background threads so the child never blocks
        // on a full pipe buffer (which would prevent it from exiting).
        let (stdin_result_rx, stdin_thread) =
            spawn_stdin_thread(child.process.child_mut(), stdin, watch)?;
        // Each of the two drain threads reports a sink stop at most once.
        let (sink_stop_tx, sink_stop_rx) = mpsc::sync_channel(2);
        let stdout_thread = match child.process.child_mut().stdout.take() {
            Some(out) => Some(spawn_sink_drain(
                out,
                stdout,
                sink_stop_tx.clone(),
                watch()?,
            )),
            None => {
                stdout.finish();
                None
            }
        };
        let stderr_thread = match child.process.child_mut().stderr.take() {
            Some(err) => Some(spawn_sink_drain(err, stderr, sink_stop_tx, watch()?)),
            None => {
                stderr.finish();
                None
            }
        };

        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        // A descendant that stays stopped would hold the child's wait until
        // the deadline; see `stopped_descendants`.
        let mut stopped_watch = stopped_threshold.map(StoppedDescendantWatch::new);
        let mut stopped_descendants: Vec<StoppedDescendant> = Vec::new();
        let mut stdin_write_error = None;
        let mut sink_stopped: Option<ProcessStopCause> = None;
        let mut termination_status = None;
        // Annotated because the only `Some(signal)` arms are Unix-only.
        let (timed_out, interrupted_signal, mut success, exit_code): (
            bool,
            Option<i32>,
            bool,
            Option<i32>,
        ) = loop {
            // Check before waiting or another cancellation cause: registration
            // may have raced a signal before the handler could fan it out.
            #[cfg(unix)]
            if let Some(signal) = child.take_signal() {
                end_group(&mut child.process, termination, signal)?;
                break (false, Some(signal), false, Some(128 + signal));
            }

            if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
                child.process.kill_and_reap();
                break (false, None, false, None);
            }
            if let Ok(cause) = sink_stop_rx.try_recv() {
                end_group(&mut child.process, termination, termination_signal())?;
                sink_stopped = Some(cause);
                break (false, None, false, None);
            }

            if let Some(rx) = stdin_result_rx.as_ref() {
                match rx.try_recv() {
                    Ok(Ok(())) => {}
                    // A backend that exits before consuming the request
                    // envelope (missing interpreter, empty shim, launcher
                    // error) closes its stdin pipe; the writer then observes
                    // EPIPE. That is not a supervisor-side failure, so fall
                    // through instead of terminating: the wait loop below
                    // reaps the child's real exit status and stderr tail,
                    // matching the non-zero-exit diagnostic instead of a bare
                    // "Broken pipe" error.
                    Ok(Err(err)) if err.kind() == std::io::ErrorKind::BrokenPipe => {}
                    Ok(Err(err)) => {
                        end_group(&mut child.process, termination, termination_signal())?;
                        stdin_write_error = Some(OrbitError::Execution(format!(
                            "failed to write process stdin: {err}"
                        )));
                        break (false, None, false, None);
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {}
                }
            }

            // The child is not reaped yet, so its pid still names it. Each
            // one is reported in the outcome.
            if let Some(watch) = stopped_watch.as_mut() {
                stopped_descendants.extend(watch.poll(child.process.pid()));
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
                .child_mut()
                .wait_timeout(wait_slice)
                .map_err(|e| OrbitError::Execution(format!("wait timeout error: {e}")))?
            {
                // The child is reaped: its pid is free for reuse from here
                // on, so a SIGINT/SIGTERM arriving before this wait returns
                // must not `killpg` whatever process group now owns that
                // number.
                child.release_process_group();

                #[cfg(unix)]
                if let Some(signal) = child.take_signal() {
                    end_group(&mut child.process, termination, signal)?;
                    break (false, Some(signal), false, Some(128 + signal));
                }

                // Child exited within the timeout. Kill its process group so
                // any orphan subprocesses still holding the pipes open are
                // reaped before the pipe workers settle below.
                let _ = child.process.signal_group(9);
                if !status.success() && status.code().is_none() {
                    termination_status = Some(status);
                }
                break (false, None, status.success(), status.code());
            }

            #[cfg(unix)]
            if let Some(signal) = child.take_signal() {
                end_group(&mut child.process, termination, signal)?;
                break (false, Some(signal), false, Some(128 + signal));
            }

            if deadline.is_some_and(|end| Instant::now() >= end) {
                end_group(&mut child.process, termination, termination_signal())?;
                break (true, None, false, None);
            }
        };
        // Every exit above has reaped the child (directly or through
        // `end_group`); stop fanning signals out to its old group before the
        // pipe workers settle below, which can outlast a pid's reuse.
        child.mark_reaped();

        // The process group is dead, so its pipe ends are closed and the
        // workers normally hit EOF at once. Only a holder outside the group
        // keeps one open; `settle` stops such workers after the budget, so
        // the joins below are bounded either way.
        let drain_stopped = drain_stop.settle(drain_budget);
        let stdout = stdout_thread
            .map(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        let stderr = stderr_thread
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
            // A BrokenPipe here means the write raced the child's own exit
            // and observed EPIPE only after `wait_timeout` above already
            // reaped the real exit status, or that the writer was stopped
            // because only a holder outside the process group still had the
            // pipe. Treat it the same as the in-loop EPIPE case and let the
            // already-captured exit status and stderr tail stand.
            return Err(OrbitError::Execution(format!(
                "failed to write process stdin: {err}"
            )));
        }

        // Retain the actual child status when it has no exit code (a signal
        // on Unix). Otherwise even an empty-stderr SIGKILL is just an
        // unexplained failure to callers.
        let mut stop_causes: Vec<ProcessStopCause> = termination_status
            .map(|status| ProcessStopCause::TerminationStatus { status })
            .into_iter()
            .collect();
        // A sink stop looks like a bare failure otherwise (exit code `None`,
        // often an empty stderr). The workers have joined, so also collect
        // stops sent while the child was exiting or the pipes were settling.
        // Truncated output must fail even if the child itself exited
        // successfully.
        for cause in sink_stopped.into_iter().chain(sink_stop_rx.try_iter()) {
            success = false;
            stop_causes.push(cause);
        }
        #[cfg(unix)]
        if !timed_out && let Some(signal) = interrupted_signal {
            stop_causes.push(ProcessStopCause::InterruptedSignal { signal });
        }
        #[cfg(not(unix))]
        let _ = interrupted_signal;

        Ok(Outcome {
            success,
            exit_code,
            stdout,
            stderr,
            timed_out,
            stop_causes,
            stopped_descendants,
            drain_stopped,
        })
    }
}

fn spawn_stdin_thread(
    child: &mut Child,
    stdin: Stdin,
    watch: impl FnOnce() -> Result<StopWatch, OrbitError>,
) -> Result<StdinWorker, OrbitError> {
    match stdin {
        Stdin::Payload(bytes) => {
            let pipe = child.stdin.take().ok_or_else(|| {
                OrbitError::Execution("stdin requested but no stdin pipe available".to_string())
            })?;
            // The single stdin writer sends one completion result.
            let (tx, rx) = mpsc::sync_channel(1);
            let handle = spawn_stdin_write(pipe, bytes, tx, watch()?);
            Ok((Some(rx), Some(handle)))
        }
        Stdin::Null => Ok((None, None)),
    }
}

fn receive_stdin_result(
    stdin_result_rx: Option<StdinResultReceiver>,
) -> Option<std::io::Result<()>> {
    stdin_result_rx.and_then(|rx| rx.recv().ok())
}

#[cfg(unix)]
fn termination_signal() -> i32 {
    libc::SIGTERM
}

#[cfg(not(unix))]
fn termination_signal() -> i32 {
    15
}

/// End the group under `termination`, signalling `signal` first when it
/// allows a grace period. Reaps the child on success.
fn end_group(
    process: &mut OwnedChild,
    termination: Termination,
    signal: i32,
) -> Result<(), OrbitError> {
    match termination {
        Termination::KillNow => {
            process.kill_and_reap();
            Ok(())
        }
        Termination::Grace(grace) => process
            .terminate_with_signal(signal, grace, WAIT_POLL_INTERVAL)
            .map_err(|error| OrbitError::Execution(format!("failed waiting for process: {error}"))),
    }
}
