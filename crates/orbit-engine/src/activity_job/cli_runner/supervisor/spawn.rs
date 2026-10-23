use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use orbit_common::process::build_budget::{WAIT_DIRECTORY_ENV, read_waits};
use orbit_common::process::stopped_descendants::StoppedDescendant;
use orbit_common::process::supervisor::{
    Periodic, ProcessStopCause, Stdin, Stream, SupervisedChild, Supervision, Termination,
};

use super::super::super::dispatcher::ResolvedSandbox;
use super::super::spawn::{SpawnError, SpawnedChild, spawn_child_with_optional_sandbox};
use super::capture::{
    CapturedOutput, OutputProgress, RollingOutputCapture, default_output_capture_limit,
    finish_captured_output,
};
use super::output::{OutputLineContext, TracedCaptureSink};

type SpawnOutput = (
    CapturedOutput,
    CapturedOutput,
    Option<i32>,
    Duration,
    bool,
    bool,
);

/// Newest stdout bytes a progress sample carries. Large enough for one
/// provider event holding a long assistant message.
const PROGRESS_WINDOW_BYTES: usize = 64 * 1024;

/// How long output readers may keep draining after the provider's process
/// group is gone. Only a holder outside the group (a helper the agent left in
/// its own session) keeps a pipe open that long.
pub(super) const OUTPUT_DRAIN_BUDGET: Duration = Duration::from_millis(500);

#[cfg(test)]
thread_local! {
    static FIXTURE_DRAIN_BUDGET: std::cell::Cell<Duration> = const { std::cell::Cell::new(OUTPUT_DRAIN_BUDGET) };
}

/// Bound escaped-pipe cleanup in stdin fixtures without changing the production guard.
#[cfg(test)]
pub(super) fn with_output_drain_budget<T>(budget: Duration, operation: impl FnOnce() -> T) -> T {
    struct Restore(Duration);
    impl Drop for Restore {
        fn drop(&mut self) {
            FIXTURE_DRAIN_BUDGET.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(FIXTURE_DRAIN_BUDGET.with(|slot| slot.replace(budget)));
    operation()
}

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
    /// A child [`spawn_for_supervision`] already spawned, for a caller that
    /// works on it before supervision starts. Without it, the child is
    /// spawned from the request fields.
    pub(in crate::activity_job::cli_runner) spawned_child: Option<SpawnedChild>,
    /// End the run when stdout, stderr and the process tree's CPU time have
    /// all stayed flat for this long. `None` or zero leaves only the
    /// wall-clock deadline.
    pub(in crate::activity_job::cli_runner) stall_window: Option<Duration>,
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
        spawned_child,
        stall_window,
    } = request;

    let started = Instant::now();
    let deadline_start = supervision_now();
    let spawned = match spawned_child {
        Some(spawned) => spawned,
        None => spawn_for_supervision(program, args, env, cwd, sandbox, trace.provider)?,
    };
    // The mount plan outlives supervision, which reaps the child (or kills
    // and reaps it on unwind) before returning.
    let SpawnedChild {
        child,
        _linux_mount_plan: mount_plan,
    } = spawned;
    let mut child = SupervisedChild::adopt(child);
    let stdin = if child.child_mut().stdin.is_some() {
        provider_stdin(program, stdin_bytes)?
    } else {
        Stdin::Null
    };

    let output_limit = output_capture_limit.unwrap_or_else(default_output_capture_limit);
    let stdout_buf = Arc::new(Mutex::new(RollingOutputCapture::new(output_limit)));
    let stderr_buf = Arc::new(Mutex::new(RollingOutputCapture::new(output_limit)));
    let dispatch = tracing::dispatcher::get_default(Clone::clone);
    let line_context = |stream| OutputLineContext {
        provider: trace.provider.to_string(),
        stream,
        job_run_id: trace.job_run_id.to_string(),
        task_id: trace.task_id.map(ToString::to_string),
        cwd: trace.cwd.map(ToString::to_string),
        dispatch: dispatch.clone(),
    };

    let deadline = deadline_start + timeout;
    let wait_directory = env
        .iter()
        .rev()
        .find(|(name, _)| name == WAIT_DIRECTORY_ENV)
        .map(|(_, value)| Path::new(value));
    // Build-budget queue credit extends the deadline while the provider runs.
    // The deadline is kept on the supervision clock and handed to the
    // supervisor as the matching real instant.
    let extendable_deadline = || {
        let credit = wait_directory.map_or(0, |directory| {
            read_waits(
                directory,
                timeout.as_millis().try_into().unwrap_or(u64::MAX),
            )
            .deadline_extension_ms
        });
        let deadline = deadline + Duration::from_millis(credit);
        let (real, clock) = (Instant::now(), supervision_now());
        match deadline.checked_duration_since(clock) {
            Some(remaining) => real + remaining,
            None => real.checked_sub(clock - deadline).unwrap_or(real),
        }
    };
    let root_pid = child.child_mut().id();
    let mut stall_watch = stall_window
        .filter(|window| !window.is_zero())
        .map(|window| super::stall::StallWatch::new(window, root_pid));
    // Read the interval before the callback borrows the watch.
    let stall_interval = stall_watch
        .as_ref()
        .map(super::stall::StallWatch::sample_interval);
    let mut last_progress: Option<Instant> = None;
    let mut sample_supervision = || {
        if let Some(progress) = on_progress.as_ref() {
            let due = last_progress.is_none_or(|sampled| sampled.elapsed() >= progress.interval);
            if due && let Ok(capture) = stdout_buf.lock() {
                let sample = OutputProgress {
                    observed_bytes: capture.observed_bytes,
                    recent: capture.recent(PROGRESS_WINDOW_BYTES),
                };
                drop(capture);
                (progress.report)(&sample);
                last_progress = Some(Instant::now());
            }
        }
        let watch = stall_watch.as_mut()?;
        let (Ok(stdout), Ok(stderr)) = (stdout_buf.lock(), stderr_buf.lock()) else {
            return None;
        };
        let decision = watch.poll(stdout.observed_bytes, stderr.observed_bytes);
        drop(stdout);
        drop(stderr);
        decision
    };
    let periodic_interval = match (
        stall_interval,
        on_progress.as_ref().map(|progress| progress.interval),
    ) {
        (Some(stall), Some(progress)) => Some(stall.min(progress)),
        (Some(stall), None) => Some(stall),
        (None, Some(progress)) => Some(progress),
        (None, None) => None,
    };
    #[cfg(test)]
    let output_drain_budget = FIXTURE_DRAIN_BUDGET.with(std::cell::Cell::get);
    #[cfg(not(test))]
    let output_drain_budget = OUTPUT_DRAIN_BUDGET;
    let outcome = child.supervise(Supervision {
        stdin,
        stdout: Stream::Sink(Box::new(TracedCaptureSink::new(
            Arc::clone(&stdout_buf),
            line_context("stdout"),
        ))),
        stderr: Stream::Sink(Box::new(TracedCaptureSink::new(
            Arc::clone(&stderr_buf),
            line_context("stderr"),
        ))),
        timeout: None,
        clock: None,
        extendable_deadline: Some(&extendable_deadline),
        termination: Termination::KillNow,
        cancelled: None,
        // Whether a provider run should forward SIGINT/SIGTERM to its group
        // is open; the worker's own shutdown and Core's recovery handle it.
        intercept_signals: false,
        drain_budget: output_drain_budget,
        cap_drain_at_deadline: false,
        require_eof: false,
        stopped_descendants: stopped_descendants
            .as_ref()
            .map(|reporter| reporter.threshold),
        on_stopped_descendant: stopped_descendants.as_ref().map(|reporter| reporter.report),
        on_spawn,
        every: periodic_interval.map(|interval| Periodic {
            interval,
            run: &mut sample_supervision,
        }),
    });
    drop(mount_plan);

    // Supervision has joined both sinks' workers, so the captures are final.
    let stdout = finish_captured_output(&stdout_buf, output_limit);
    let stderr = finish_captured_output(&stderr_buf, output_limit);
    // Supervision failures are host-side and not clearly deterministic:
    // leave them retryable.
    let outcome =
        outcome.map_err(|err| SpawnError::transient(format!("supervise {program}: {err}")))?;
    let stalled = outcome.stop_causes.iter().any(|cause| {
        matches!(
            cause,
            ProcessStopCause::Requested {
                reason: super::stall::STALL_STOP_REASON,
            }
        )
    });
    Ok((
        stdout,
        stderr,
        outcome.exit_code,
        started.elapsed(),
        outcome.timed_out,
        stalled,
    ))
}

#[cfg(unix)]
fn provider_stdin(_program: &str, bytes: &[u8]) -> Result<Stdin, SpawnError> {
    Ok(Stdin::Payload(bytes.to_vec()))
}

/// Native non-Unix stdin supervision is unsupported; Windows runs Orbit
/// through WSL2.
#[cfg(not(unix))]
fn provider_stdin(program: &str, _bytes: &[u8]) -> Result<Stdin, SpawnError> {
    Err(SpawnError::permanent(format!(
        "stdin {program}: bounded provider stdin supervision requires Unix (use WSL2 on Windows)"
    )))
}

/// Only deadline decisions use the injected clock; cleanup retains real guards.
fn supervision_now() -> Instant {
    #[cfg(feature = "test-support")]
    if let Some(now) = super::test_support::now() {
        return now;
    }
    Instant::now()
}
