//! Bounded retries and diagnostics for read-only delivery remote probes.

use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;

use super::git::{git_failure_error, git_run, timeout_recovery_error};

const PROBE_ATTEMPTS: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 250;

#[cfg(feature = "test-support")]
thread_local! {
    static BACKOFF_MS: std::cell::Cell<u64> = const { std::cell::Cell::new(INITIAL_BACKOFF_MS) };
}

/// Scale retry waits in fixtures without changing attempt counts or production defaults.
#[cfg(feature = "test-support")]
pub fn with_remote_probe_backoff<T>(milliseconds: u64, operation: impl FnOnce() -> T) -> T {
    struct Restore(u64);
    impl Drop for Restore {
        fn drop(&mut self) {
            BACKOFF_MS.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(BACKOFF_MS.with(|slot| slot.replace(milliseconds)));
    operation()
}

fn initial_backoff_ms() -> u64 {
    #[cfg(feature = "test-support")]
    {
        BACKOFF_MS.with(std::cell::Cell::get)
    }
    #[cfg(not(feature = "test-support"))]
    {
        INITIAL_BACKOFF_MS
    }
}

// PR polling has a five-second production floor. These fixtures retain
// requested delay accounting while exercising the next observation immediately;
// ordinary builds and unscoped tests still perform the original real wait.
#[cfg(feature = "test-support")]
mod fixed_wait_fixture {
    use std::{cell::RefCell, rc::Rc, time::Duration};

    type Wait = Rc<dyn Fn(Duration)>;
    thread_local! {
        static WAIT: RefCell<Option<Wait>> = const { RefCell::new(None) };
    }

    /// Replace fixed VCS retry waits on this thread for one fixture operation.
    /// Real subprocess deadlines and process cleanup retain their real clocks.
    pub fn with_vcs_retry_wait<T>(
        wait: impl Fn(Duration) + 'static,
        operation: impl FnOnce() -> T,
    ) -> T {
        struct Restore(Option<Wait>);
        impl Drop for Restore {
            fn drop(&mut self) {
                WAIT.with(|slot| slot.replace(self.0.take()));
            }
        }
        let _restore = Restore(WAIT.with(|slot| slot.replace(Some(Rc::new(wait)))));
        operation()
    }

    pub(super) fn wait(duration: Duration) -> bool {
        let wait = WAIT.with(|slot| slot.borrow().clone());
        if let Some(wait) = wait {
            wait(duration);
            true
        } else {
            false
        }
    }
}

#[cfg(feature = "test-support")]
pub use fixed_wait_fixture::with_vcs_retry_wait;

pub(crate) fn fixed_wait(duration: Duration) {
    #[cfg(feature = "test-support")]
    if fixed_wait_fixture::wait(duration) {
        return;
    }
    std::thread::sleep(duration);
}

pub(super) struct RemoteBranchProbe {
    pub(super) sha: Option<String>,
    pub(super) attempts: u32,
    pub(super) elapsed_ms: u64,
    pub(super) waited_ms: u64,
}

/// Probe `branch` at `remote`, the effective publication destination.
///
/// `remote` must be the destination `git push -- origin <branch>` would
/// write (the single effective push URL), not the name `origin`: a plain
/// `ls-remote origin` reads the fetch URL, which can name a different
/// repository than the push URL [ORB-17671].
pub(super) fn pre_push_remote_branch(
    repo_root: &Path,
    branch: &str,
    remote: &str,
) -> Result<RemoteBranchProbe, OrbitError> {
    let reference = format!("refs/heads/{branch}");
    let args = ["ls-remote", "--heads", remote, &reference];
    let probe = remote_ref_output(repo_root, &args, "git_push pre-push")?;
    Ok(RemoteBranchProbe {
        sha: probe.stdout.split_whitespace().next().map(str::to_owned),
        attempts: probe.attempts,
        elapsed_ms: probe.elapsed_ms,
        waited_ms: probe.waited_ms,
    })
}

pub(super) struct RemoteRefProbe {
    pub(super) stdout: String,
    pub(super) attempts: u32,
    pub(super) elapsed_ms: u64,
    pub(super) waited_ms: u64,
}

/// Retry only supervisor deadlines, never a completed Git refusal. Each child
/// retains the activity's Git budget and secured environment; backoff is bounded
/// to 250 ms then 500 ms. The probe acquires no Orbit repository lock. Callers
/// supply only read-only `ls-remote` arguments and keep their own ref parsing.
pub(super) fn remote_ref_output(
    repo_root: &Path,
    args: &[&str],
    stage: &str,
) -> Result<RemoteRefProbe, OrbitError> {
    let started = Instant::now();
    let io_before = io_stall_total();
    let mut lock_observed = false;
    let mut waited_ms = 0;
    for attempt in 1..=PROBE_ATTEMPTS {
        let outcome = git_run(repo_root, args)?;
        if !outcome.timed_out {
            if !outcome.success {
                return Err(git_failure_error(
                    repo_root,
                    args,
                    outcome.exit_code,
                    &outcome.stderr,
                ));
            }
            return Ok(RemoteRefProbe {
                stdout: outcome.stdout,
                attempts: attempt,
                elapsed_ms: elapsed_ms(started),
                waited_ms,
            });
        }
        let stderr = outcome.stderr.to_ascii_lowercase();
        lock_observed |= stderr.contains("could not lock")
            || stderr.contains("cannot lock")
            || (stderr.contains(".lock") && stderr.contains("file exists"));
        let io = io_contention(io_before, io_stall_total());
        let elapsed_ms = elapsed_ms(started);
        if attempt == PROBE_ATTEMPTS {
            let lock = if lock_observed {
                "observed in Git stderr"
            } else {
                "not observed in Git stderr"
            };
            return Err(timeout_recovery_error(
                outcome.timeout_ms,
                format!(
                    "{stage} ls-remote probe timed out after {attempt} attempts; \
                     elapsed={elapsed_ms}ms, per-attempt timeout={}ms, backoff={waited_ms}ms; \
                     local lock contention={lock} (probe acquires no Orbit lock); \
                     IO contention={io}; git {} in '{}': {}",
                    outcome.timeout_ms,
                    args.join(" "),
                    repo_root.display(),
                    outcome.stderr.trim(),
                ),
            ));
        }
        let backoff_ms = initial_backoff_ms() << (attempt - 1);
        tracing::warn!(
            stage,
            attempt,
            elapsed_ms,
            backoff_ms,
            lock_observed,
            io_contention = %io,
            "retrying timed-out ls-remote probe"
        );
        std::thread::sleep(Duration::from_millis(backoff_ms));
        waited_ms += backoff_ms;
    }
    unreachable!("the last probe attempt returns its outcome")
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

/// Linux PSI counts microseconds with at least one host task stalled on IO.
/// It is host-wide evidence during the probe, not attribution to this child.
#[cfg(target_os = "linux")]
fn io_stall_total() -> Option<u64> {
    let pressure = std::fs::read_to_string("/proc/pressure/io").ok()?;
    pressure
        .lines()
        .find(|line| line.starts_with("some "))?
        .split_whitespace()
        .find_map(|field| field.strip_prefix("total="))?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn io_stall_total() -> Option<u64> {
    None
}

fn io_contention(before: Option<u64>, after: Option<u64>) -> String {
    match before
        .zip(after)
        .and_then(|(before, after)| after.checked_sub(before))
    {
        Some(0) => "not observed (host IO PSI stall counter did not advance)".to_string(),
        Some(stalled_us) => format!(
            "observed (host IO PSI stalled {stalled_us}us during probe; not attributed to Git)"
        ),
        None => "unknown (host IO PSI unavailable)".to_string(),
    }
}
