//! Bounded retries and diagnostics for the read-only pre-push remote probe.

use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;

use super::git::{git_failure_error, git_run, timeout_recovery_error};

const PROBE_ATTEMPTS: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 250;

pub(super) struct RemoteBranchProbe {
    pub(super) sha: Option<String>,
    pub(super) attempts: u32,
    pub(super) elapsed_ms: u64,
    pub(super) waited_ms: u64,
}

/// Retry only supervisor deadlines, never a completed Git refusal. Each child
/// retains the activity's Git budget and secured environment; backoff is bounded
/// to 250 ms then 500 ms. The probe acquires no Orbit repository lock.
pub(super) fn pre_push_remote_branch(
    repo_root: &Path,
    branch: &str,
) -> Result<RemoteBranchProbe, OrbitError> {
    let reference = format!("refs/heads/{branch}");
    let args = ["ls-remote", "--heads", "origin", &reference];
    let started = Instant::now();
    let io_before = io_stall_total();
    let mut lock_observed = false;
    let mut waited_ms = 0;
    for attempt in 1..=PROBE_ATTEMPTS {
        let outcome = git_run(repo_root, &args)?;
        if !outcome.timed_out {
            if !outcome.success {
                return Err(git_failure_error(
                    repo_root,
                    &args,
                    outcome.exit_code,
                    &outcome.stderr,
                ));
            }
            return Ok(RemoteBranchProbe {
                sha: outcome.stdout.split_whitespace().next().map(str::to_owned),
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
                    "git_push pre-push ls-remote probe timed out after {attempt} attempts; \
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
        let backoff_ms = INITIAL_BACKOFF_MS << (attempt - 1);
        tracing::warn!(
            branch,
            attempt,
            elapsed_ms,
            backoff_ms,
            lock_observed,
            io_contention = %io,
            "retrying timed-out pre-push ls-remote probe"
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
