//! The supervisor's wait loop. A deadline ends a still-running child, not a
//! completed child whose last timed wait was stale when the waiter resumed.

use std::process::Child;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::super::OwnedChild;
use super::super::stopped_descendants::{StoppedDescendant, StoppedDescendantWatch};
use super::drain::{DrainEvent, DrainStop, PipeFd, StopWatch, spawn_sink_drain, spawn_stdin_write};
use super::seam;
#[cfg(unix)]
use super::signal_guard::SignalHandlerGuard;
use super::{Outcome, ProcessStopCause, Stdin, Stream, Supervision, Termination};
use crate::OrbitError;

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long supervision trusts an extendable deadline's last answer.
const DEADLINE_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

type StdinResultReceiver = Receiver<std::io::Result<()>>;
type StdinWorker = (Option<StdinResultReceiver>, Option<JoinHandle<()>>);
type OutputWorker = Option<JoinHandle<std::io::Result<Vec<u8>>>>;

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

    /// Tear down descendants after observing the leader's exit. Capped
    /// drains signal immediately and spend their remaining budget on pipes;
    /// the owned handle's additional group-drain wait would exceed that bound.
    fn finish_reaped_group(&mut self, cap_drain_at_deadline: bool) {
        if cap_drain_at_deadline {
            let _ = self.process.signal_group(9);
            self.mark_reaped();
        } else {
            self.process.kill_and_reap();
        }
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
    /// Returns pipe setup/read failures, handler or wait failures, output worker
    /// panics and stdin write failures other than a broken pipe. The child's
    /// group is killed, the child reaped and every pipe worker finished before
    /// any error returns.
    pub fn supervise(mut self, run: Supervision<'_>) -> Result<Outcome, OrbitError> {
        let Supervision {
            stdin,
            stdout,
            stderr,
            timeout,
            clock,
            extendable_deadline,
            termination,
            cancelled,
            intercept_signals,
            drain_budget,
            cap_drain_at_deadline,
            require_eof,
            stopped_descendants: stopped_threshold,
            on_stopped_descendant,
            on_spawn,
            mut every,
        } = run;
        if every
            .as_ref()
            .is_some_and(|periodic| periodic.interval.is_zero())
        {
            return Err(OrbitError::Execution(
                "process supervision periodic interval must be nonzero".to_string(),
            ));
        }
        let child = &mut self;

        if let Some(on_spawn) = on_spawn {
            on_spawn(child.process.pid());
        }

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
        #[cfg(feature = "test-support")]
        let fixture_clock = seam::clock();
        #[cfg(feature = "test-support")]
        let clock = clock.or(fixture_clock.as_deref());
        let mut deadline = Deadline::new(timeout, extendable_deadline, clock);
        let drain_stop = DrainStop::new();
        #[cfg(all(test, unix))]
        seam::inject_supervision_failure(
            seam::SupervisionFailure::PreparePipes,
            child.process.pid(),
        )?;

        // Drain stdout/stderr in background threads so the child never blocks
        // on a full pipe buffer (which would prevent it from exiting).
        let (stdin_result_rx, stdin_thread) =
            spawn_stdin_thread(child.process.child_mut(), stdin, || drain_stop.watch())?;
        // Each drain reports either a sink stop or a capture failure once.
        let (drain_event_tx, drain_event_rx) = mpsc::sync_channel(2);
        #[cfg(all(test, unix))]
        let pid = child.process.pid();
        let output_watch = || {
            let watch = drain_stop.watch();
            #[cfg(all(test, unix))]
            let watch = watch.with_output_failure(pid);
            watch
        };
        let stdout_thread = attach_stream(
            child.process.child_mut().stdout.take(),
            stdout,
            drain_event_tx.clone(),
            output_watch,
        );
        let stderr_thread = attach_stream(
            child.process.child_mut().stderr.take(),
            stderr,
            drain_event_tx,
            output_watch,
        );

        let mut next_periodic = every
            .as_ref()
            .map(|periodic| Instant::now() + periodic.interval);
        // A descendant that stays stopped would hold the child's wait until
        // the deadline; see `stopped_descendants`.
        let mut stopped_watch = stopped_threshold.map(StoppedDescendantWatch::new);
        let mut stopped_descendants: Vec<StoppedDescendant> = Vec::new();
        let mut stdin_write_error = None;
        let mut sink_stopped: Option<ProcessStopCause> = None;
        let mut check_stopped = None;
        let mut termination_status = None;
        let mut completed_status = None;
        // An error leaves the child to the cleanup below, so the pipe
        // workers are finished before it returns.
        let waited: Result<(bool, Option<i32>, bool, Option<i32>), OrbitError> = (|| loop {
            // Check before waiting or another cancellation cause: registration
            // may have raced a signal before the handler could fan it out.
            #[cfg(unix)]
            if let Some(signal) = child.take_signal() {
                end_group(&mut child.process, termination, signal)?;
                return Ok((false, Some(signal), false, Some(128 + signal)));
            }

            if cancelled.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst)) {
                child.process.kill_and_reap();
                return Ok((false, None, false, None));
            }
            match drain_event_rx.try_recv() {
                Ok(DrainEvent::SinkStop(cause)) => {
                    end_group(&mut child.process, termination, termination_signal())?;
                    sink_stopped = Some(cause);
                    return Ok((false, None, false, None));
                }
                Ok(DrainEvent::CaptureFailed) => {
                    // The joined worker below supplies the original I/O error.
                    return Err(OrbitError::Execution(
                        "process output capture failed".to_string(),
                    ));
                }
                Err(_) => {}
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
                        return Ok((false, None, false, None));
                    }
                    Err(TryRecvError::Empty) => {}
                    Err(TryRecvError::Disconnected) => {}
                }
            }

            // The child is not reaped yet, so its pid still names it. Each
            // one is reported as it is ended, and in the outcome.
            if completed_status.is_none()
                && let Some(watch) = stopped_watch.as_mut()
            {
                for stopped in watch.poll(child.process.pid()) {
                    if let Some(report) = on_stopped_descendant {
                        report(&stopped);
                    }
                    stopped_descendants.push(stopped);
                }
            }

            let now = Instant::now();
            let mut wait_slice = WAIT_POLL_INTERVAL;
            if let Some(remaining) = deadline.clock_remaining() {
                wait_slice = wait_slice.min(remaining);
            } else if let Some(end) = deadline.current() {
                wait_slice = wait_slice.min(end.saturating_duration_since(now));
            }
            if completed_status.is_none()
                && let Some(next) = next_periodic
            {
                wait_slice = wait_slice.min(next.saturating_duration_since(now));
            }

            if completed_status.is_none() {
                #[cfg(all(test, unix))]
                inject_supervision_failure(SupervisionFailure::Wait)?;
                #[cfg(all(test, unix))]
                seam::inject_supervision_failure(
                    seam::SupervisionFailure::Wait,
                    child.process.pid(),
                )?;
                completed_status = seam::wait_timeout(child.process.child_mut(), wait_slice)
                    .map_err(|e| OrbitError::Execution(format!("wait timeout error: {e}")))?;
                if completed_status.is_some() {
                    // Stop signal fan-out as soon as the leader is reaped,
                    // even when completion still waits for its output EOF.
                    child.release_process_group();
                }
            } else if !streams_finished(&stdout_thread, &stderr_thread) {
                std::thread::sleep(wait_slice);
            }
            if completed_status.is_none() && deadline.passed() {
                // A timed wait's None describes its last poll. The waiter can
                // lose the CPU after that poll while the child exits and the
                // deadline passes. Recheck without another wait before calling
                // this a timeout; otherwise even a completed no-op is killed
                // and reported as timed out under scheduler contention.
                completed_status =
                    child.process.child_mut().try_wait().map_err(|e| {
                        OrbitError::Execution(format!("final child wait error: {e}"))
                    })?;
                if completed_status.is_some() {
                    child.release_process_group();
                }
            }
            if let Some(status) = completed_status
                .filter(|_| !require_eof || streams_finished(&stdout_thread, &stderr_thread))
            {
                #[cfg(unix)]
                if let Some(signal) = child.take_signal() {
                    end_group(&mut child.process, termination, signal)?;
                    return Ok((false, Some(signal), false, Some(128 + signal)));
                }

                // Tear down descendants as soon as the chosen completion
                // mode finishes, before settling the pipe workers below.
                child.finish_reaped_group(cap_drain_at_deadline);
                if !status.success() && status.code().is_none() {
                    termination_status = Some(status);
                }
                return Ok((false, None, status.success(), status.code()));
            }

            #[cfg(unix)]
            if let Some(signal) = child.take_signal() {
                end_group(&mut child.process, termination, signal)?;
                return Ok((false, Some(signal), false, Some(128 + signal)));
            }

            if deadline.passed() {
                if completed_status.is_some() {
                    // The leader already exited in EOF completion mode. Its
                    // descendants get no additional termination grace.
                    child.finish_reaped_group(cap_drain_at_deadline);
                } else {
                    end_group(&mut child.process, termination, termination_signal())?;
                }
                return Ok((true, None, false, None));
            }

            if completed_status.is_none()
                && let (Some(periodic), Some(next)) = (every.as_mut(), next_periodic)
            {
                let now = Instant::now();
                if now >= next {
                    if let Some(cause) = (periodic.run)() {
                        end_group(&mut child.process, termination, termination_signal())?;
                        check_stopped = Some(cause);
                        return Ok((false, None, false, None));
                    }
                    next_periodic = Some(Instant::now() + periodic.interval);
                }
            }
        })();
        // Every successful exit above has reaped the child (directly or
        // through `end_group`); an error leaves that to this kill. Stop
        // fanning signals out to its old group before the pipe workers settle
        // below, which can outlast a pid's reuse.
        if waited.is_err() {
            child.process.kill_and_reap();
        }
        child.mark_reaped();

        // The process group is dead, so its pipe ends are closed and the
        // workers normally hit EOF at once. Only a holder outside the group
        // keeps one open; `settle` stops such workers after the budget, so
        // the joins below are bounded either way.
        let drain_budget = if cap_drain_at_deadline {
            deadline
                .remaining()
                .map_or(drain_budget, |left| drain_budget.min(left))
        } else {
            drain_budget
        };
        let drain_stopped = drain_stop.settle(drain_budget);
        // Join all workers before propagating any of their errors.
        let stdout = join_output(stdout_thread, "stdout");
        let stderr = join_output(stderr_thread, "stderr");
        let stdin_thread_panicked = stdin_thread.map(|h| h.join().is_err()).unwrap_or(false);

        let stdout = stdout?;
        let stderr = stderr?;
        let (timed_out, interrupted_signal, mut success, exit_code) = waited?;
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
        for cause in check_stopped.into_iter().chain(sink_stopped).chain(
            drain_event_rx.try_iter().filter_map(|event| match event {
                DrainEvent::SinkStop(cause) => Some(cause),
                DrainEvent::CaptureFailed => None, // The joined result already checked this.
            }),
        ) {
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
            status: completed_status,
            stdout,
            stderr,
            timed_out,
            stop_causes,
            stopped_descendants,
            drain_stopped,
        })
    }
}

/// The run's deadline: fixed from the timeout, or the caller's extendable
/// one, cached for [`DEADLINE_REFRESH_INTERVAL`].
struct Deadline<'a> {
    fixed: Option<Instant>,
    clock: Option<(Instant, Duration, &'a dyn Fn() -> Instant)>,
    extendable: Option<&'a dyn Fn() -> Instant>,
    cached: Option<(Instant, Instant)>,
}

impl<'a> Deadline<'a> {
    fn new(
        timeout: Option<Duration>,
        extendable: Option<&'a dyn Fn() -> Instant>,
        clock: Option<&'a dyn Fn() -> Instant>,
    ) -> Self {
        let clock = if extendable.is_none() {
            clock
                .zip(timeout)
                .map(|(clock, limit)| (clock(), limit, clock))
        } else {
            None
        };
        let mut deadline = Self {
            fixed: timeout
                .filter(|_| clock.is_none())
                .map(|timeout| Instant::now() + timeout),
            clock,
            extendable,
            cached: None,
        };
        deadline.current();
        deadline
    }

    /// The deadline as last read, refreshed when the read is stale.
    fn current(&mut self) -> Option<Instant> {
        let Some(read) = self.extendable else {
            return self.fixed;
        };
        let now = Instant::now();
        match self.cached {
            Some((deadline, read_at)) if now < read_at + DEADLINE_REFRESH_INTERVAL => {
                Some(deadline)
            }
            _ => Some(self.refresh(read, now)),
        }
    }

    /// Remaining timeout when the caller supplied its own clock.
    fn clock_remaining(&self) -> Option<Duration> {
        self.clock.map(|(origin, limit, clock)| {
            limit.saturating_sub(clock().saturating_duration_since(origin))
        })
    }

    /// Time left under either the caller clock or the wall-clock deadline.
    fn remaining(&mut self) -> Option<Duration> {
        self.clock_remaining().or_else(|| {
            self.current()
                .map(|end| end.saturating_duration_since(Instant::now()))
        })
    }

    /// Whether the deadline has passed, reading an extendable one afresh
    /// before saying so.
    fn passed(&mut self) -> bool {
        if let Some(remaining) = self.clock_remaining() {
            return remaining.is_zero();
        }
        match self.current() {
            Some(end) if Instant::now() >= end => match self.extendable {
                Some(read) => {
                    let end = self.refresh(read, Instant::now());
                    Instant::now() >= end
                }
                None => true,
            },
            _ => false,
        }
    }

    fn refresh(&mut self, read: &dyn Fn() -> Instant, now: Instant) -> Instant {
        let deadline = read();
        self.cached = Some((deadline, now));
        deadline
    }
}

fn streams_finished(stdout: &OutputWorker, stderr: &OutputWorker) -> bool {
    stdout.as_ref().is_none_or(JoinHandle::is_finished)
        && stderr.as_ref().is_none_or(JoinHandle::is_finished)
}

fn attach_stream<R>(
    pipe: Option<R>,
    stream: Stream,
    event_tx: mpsc::SyncSender<DrainEvent>,
    watch: impl FnOnce() -> StopWatch,
) -> OutputWorker
where
    R: std::io::Read + PipeFd + Send + 'static,
{
    match stream {
        Stream::Redirected => {
            drop(pipe);
            None
        }
        Stream::Sink(sink) => match pipe {
            Some(pipe) => Some(spawn_sink_drain(pipe, sink, event_tx, watch())),
            None => {
                sink.finish();
                None
            }
        },
    }
}

fn join_output(worker: OutputWorker, stream: &str) -> Result<Vec<u8>, OrbitError> {
    let Some(worker) = worker else {
        return Ok(Vec::new());
    };
    worker
        .join()
        .map_err(|_| OrbitError::Execution(format!("{stream} capture thread panicked")))?
        .map_err(|err| OrbitError::Execution(format!("failed to capture process {stream}: {err}")))
}

fn spawn_stdin_thread(
    child: &mut Child,
    stdin: Stdin,
    watch: impl FnOnce() -> StopWatch,
) -> Result<StdinWorker, OrbitError> {
    match stdin {
        Stdin::Payload(bytes) => {
            let pipe = child.stdin.take().ok_or_else(|| {
                OrbitError::Execution("stdin requested but no stdin pipe available".to_string())
            })?;
            // The single stdin writer sends one completion result.
            let (tx, rx) = mpsc::sync_channel(1);
            let handle = spawn_stdin_write(pipe, bytes, tx, watch());
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
