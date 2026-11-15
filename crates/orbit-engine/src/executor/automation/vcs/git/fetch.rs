use std::cell::Cell;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use super::command::{BaseSyncMode, git_command_success, git_failure_error, git_output, git_run};
use super::timeout::{
    GIT_DEADLINE, GitTimeoutBudget, GitTimeoutBudgetGuard, git_timeout_error,
    timeout_recovery_error,
};
use orbit_common::OrbitError;
use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, GIT_FETCH_LOCK_HOLD_LIMIT, GIT_REMOTE_TIMEOUT, git_common_dir,
    git_fetch_cas_retry_delay, git_fetch_lock_target, should_retry_git_ref_cas,
    with_git_fetch_lock,
};
use orbit_common::fs::io::{FileLockOptions, FileLockTimeout, with_exclusive_file_lock_options};
use orbit_types::workflow::TRANSIENT_FAILURE_MARKER;
const ORIGIN_AVAILABILITY_PROBE_TIMEOUT_MS: u64 = GIT_REMOTE_TIMEOUT.as_millis() as u64;
const ORIGIN_AVAILABILITY_PROBE_MIN_TIMEOUT_MS: u64 = 1_000;

/// Fetch `origin/<branch>` into the local remote-tracking ref.
///
/// Serializes with other Orbit-owned fetches through the git-common-dir
/// lock so linked worktrees and task-pilot prepare do not CAS-fail the
/// same `refs/remotes/origin/*` ref.
/// Timeouts and transport failures retry within the same bounded attempt
/// count, with backoff. Exhaustion carries the transient failure marker;
/// authentication, permission and missing-ref refusals remain ordinary errors.
///
/// The lock is held for at most [`GIT_FETCH_LOCK_HOLD_LIMIT`] in total across
/// attempts and backoff, so a stalled remote cannot outlast the waiters (task
/// pilot, final recovery) and turn into their lock timeouts. Each attempt gets
/// the smaller of the activity's fetch budget, [`GIT_REMOTE_TIMEOUT`] and
/// what is left of that limit; a raised `git_timeouts.fetch` cannot lengthen
/// the hold.
pub fn fetch_remote_base(repo_root: &Path, base: &str) -> Result<(), OrbitError> {
    fetch_remote_base_within(repo_root, base, GIT_FETCH_LOCK_HOLD_LIMIT)
}

pub(crate) fn fetch_remote_base_within(
    repo_root: &Path,
    base: &str,
    hold_limit: Duration,
) -> Result<(), OrbitError> {
    let branch = normalize_base_branch(base)?;
    if let Some(deadline) = GIT_DEADLINE.with(Cell::get) {
        let common = git_output(
            repo_root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                OrbitError::Execution(
                    "prepared_base_refresh_refused: fresh preparation deadline exhausted"
                        .to_string(),
                )
            })?;
        let options = orbit_common::fs::file_lock::FileLockOptions::default();
        // Use the very same shared fetch lock, with its acquisition also
        // capped by the preparation's remaining deadline.
        return orbit_common::fs::io::with_exclusive_file_lock_options(
            &orbit_common::fs::git::git_fetch_lock_target(Path::new(&common)),
            "git fetch",
            orbit_common::fs::file_lock::FileLockOptions {
                timeout: remaining.min(options.timeout),
                ..options
            },
            || fetch_remote_base_locked(repo_root, &branch, hold_limit.min(remaining)),
        );
    }
    with_git_fetch_lock(repo_root, || {
        fetch_remote_base_locked(repo_root, &branch, hold_limit)
    })
}

fn fetch_remote_base_locked(
    repo_root: &Path,
    branch: &str,
    hold_limit: Duration,
) -> Result<(), OrbitError> {
    let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    let held_since = Instant::now();
    let mut last_stderr = String::new();
    let mut last_transport_error: Option<OrbitError> = None;
    for attempt in 0..GIT_FETCH_CAS_ATTEMPTS {
        let remaining = hold_limit.saturating_sub(held_since.elapsed());
        if remaining.is_zero() {
            return Err(match last_transport_error {
                Some(error) => {
                    let message = format!(
                        "{TRANSIENT_FAILURE_MARKER} remote base fetch hit the {}ms fetch-lock hold limit after {attempt} attempts: {error}",
                        hold_limit.as_millis()
                    );
                    match error {
                        OrbitError::ExecutionTimeout { timeout_ms, .. } => {
                            timeout_recovery_error(timeout_ms, message)
                        }
                        _ => OrbitError::Execution(message),
                    }
                }
                None => OrbitError::Execution(format!(
                    "failed to fetch remote base 'origin/{branch}' in '{}': fetch-lock hold limit of {}ms reached: {last_stderr}",
                    repo_root.display(),
                    hold_limit.as_millis()
                )),
            });
        }
        let outcome = {
            let current = GitTimeoutBudget::current();
            let attempt_ms = current
                .fetch_ms
                .min(GIT_REMOTE_TIMEOUT.as_millis() as u64)
                .min(remaining.as_millis().max(1) as u64);
            let _budget = GitTimeoutBudgetGuard::install(GitTimeoutBudget {
                fetch_ms: attempt_ms,
                ..current
            });
            git_run(repo_root, &["fetch", "origin", &spec])?
        };
        if outcome.success && !outcome.timed_out {
            return Ok(());
        }
        if outcome.timed_out || is_git_transport_failure(&outcome.stderr) {
            let error = if outcome.timed_out {
                git_timeout_error(
                    repo_root,
                    &["fetch", "origin", &spec],
                    outcome.timeout_ms,
                    &outcome.stderr,
                )
            } else {
                git_failure_error(
                    repo_root,
                    &["fetch", "origin", &spec],
                    outcome.exit_code,
                    &outcome.stderr,
                )
            };
            if attempt + 1 == GIT_FETCH_CAS_ATTEMPTS {
                let message = format!(
                    "{TRANSIENT_FAILURE_MARKER} remote base fetch failed after {} attempts: {error}",
                    attempt + 1
                );
                return Err(if outcome.timed_out {
                    timeout_recovery_error(outcome.timeout_ms, message)
                } else {
                    OrbitError::Execution(message)
                });
            }
            tracing::warn!(attempt, branch, %error, "retrying remote base fetch after transport failure");
            last_transport_error = Some(error);
            let backoff = Duration::from_millis(250 * (1 << attempt));
            super::super::remote_probe::fixed_wait(
                backoff.min(hold_limit.saturating_sub(held_since.elapsed())),
            );
            continue;
        }
        last_stderr = outcome.stderr.trim().to_string();
        last_transport_error = None;
        if should_retry_git_ref_cas(attempt, &last_stderr) {
            tracing::warn!(
                attempt,
                branch,
                "retrying origin fetch after git ref update contention"
            );
            super::super::remote_probe::fixed_wait(git_fetch_cas_retry_delay());
            continue;
        }
        break;
    }

    Err(OrbitError::Execution(format!(
        "failed to fetch remote base 'origin/{branch}' in '{}': {last_stderr}",
        repo_root.display()
    )))
}

/// Match transport diagnostics narrowly: an access refusal may also mention
/// a closed connection, but must never become an inconclusive network result.
fn is_git_transport_failure(stderr: &str) -> bool {
    let text = stderr.to_ascii_lowercase();
    if [
        "authentication failed",
        "permission denied",
        "access denied",
        "could not read username",
        "terminal prompts disabled",
        "host key verification failed",
        "ssl certificate problem",
        "couldn't find remote ref",
        "repository not found",
        "not a git repository",
        "the requested url returned error: 4",
    ]
    .iter()
    .any(|refusal| text.contains(refusal))
    {
        return false;
    }
    [
        "could not resolve host",
        "could not resolve proxy",
        "could not resolve hostname",
        "unable to look up",
        "failed to connect",
        "couldn't connect to server",
        "connection refused",
        "connection timed out",
        "connection reset",
        "connection closed",
        "connection aborted",
        "network is unreachable",
        "no route to host",
        "operation timed out",
        "remote end hung up unexpectedly",
        "early eof",
    ]
    .iter()
    .any(|transport| text.contains(transport))
}

pub(in crate::executor::automation) fn resolve_worktree_start_point(
    repo_root: &Path,
    base: &str,
    sync_mode: BaseSyncMode,
) -> Result<String, OrbitError> {
    let branch = normalize_base_branch(base)?;
    match sync_mode {
        BaseSyncMode::Local => resolve_local_base_ref(repo_root, &branch),
        BaseSyncMode::Remote => {
            fetch_remote_base(repo_root, &branch)?;
            resolve_remote_base_ref(repo_root, &branch)
        }
    }
}

/// Resolve the start point used before a task is admitted. Only this boundary
/// parks transport outages as held runs; landing and post-admission callers
/// keep their existing failure and recovery behavior.
pub(in crate::executor::automation) fn resolve_worktree_start_point_for_admission(
    repo_root: &Path,
    base: &str,
    sync_mode: BaseSyncMode,
) -> Result<String, OrbitError> {
    match sync_mode {
        BaseSyncMode::Local => resolve_worktree_start_point(repo_root, base, sync_mode),
        BaseSyncMode::Remote => {
            let branch = normalize_base_branch(base)?;
            fetch_remote_base_for_worktree_setup(repo_root, &branch)?;
            resolve_remote_base_ref(repo_root, &branch)
        }
    }
}

/// Shorten the pre-admission fetch hold and the wait to acquire its lock.
///
/// Production callers keep [`GIT_FETCH_LOCK_HOLD_LIMIT`] and the default
/// file-lock timeout. A fixture installs both on the thread that runs
/// worktree setup, the same limits [`fetch_remote_base_within`] already takes
/// for the hold.
#[cfg(feature = "test-support")]
pub fn with_worktree_setup_fetch_limits<T>(
    hold_limit: Duration,
    lock_wait: Duration,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<WorktreeSetupFetchLimits>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WORKTREE_SETUP_FETCH_LIMITS.with(|slot| slot.set(self.0.take()));
        }
    }
    let _restore = Restore(WORKTREE_SETUP_FETCH_LIMITS.with(|slot| {
        slot.replace(Some(WorktreeSetupFetchLimits {
            hold_limit,
            lock_wait,
        }))
    }));
    operation()
}

#[cfg(feature = "test-support")]
#[derive(Clone, Copy)]
struct WorktreeSetupFetchLimits {
    hold_limit: Duration,
    lock_wait: Duration,
}

#[cfg(feature = "test-support")]
thread_local! {
    static WORKTREE_SETUP_FETCH_LIMITS: Cell<Option<WorktreeSetupFetchLimits>> =
        const { Cell::new(None) };
}

fn worktree_setup_fetch_limits() -> (Duration, Duration) {
    #[cfg(feature = "test-support")]
    if let Some(limits) = WORKTREE_SETUP_FETCH_LIMITS.with(Cell::get) {
        return (limits.hold_limit, limits.lock_wait);
    }
    (
        GIT_FETCH_LOCK_HOLD_LIMIT,
        orbit_common::fs::file_lock::DEFAULT_FILE_LOCK_TIMEOUT,
    )
}

/// Fetch origin for pre-admission setup while retaining enough lock-timeout
/// detail to tell an origin outage from ordinary lock contention.
fn fetch_remote_base_for_worktree_setup(repo_root: &Path, branch: &str) -> Result<(), OrbitError> {
    let common_dir = git_common_dir(repo_root)?;
    let lock_target = git_fetch_lock_target(&common_dir);
    let (hold_limit, lock_wait) = worktree_setup_fetch_limits();
    let mut options = FileLockOptions::default();
    options.timeout = lock_wait;
    // A short fixture wait must not leave the contention warning past the
    // deadline. The production pair (30 s timeout, 3 s warning) is unchanged.
    options.warn_after = options.warn_after.min(lock_wait);
    let result = with_exclusive_file_lock_options(&lock_target, "git fetch", options, || {
        fetch_remote_base_locked(repo_root, branch, hold_limit)
            .map_err(WorktreeFetchLockError::Fetch)
    });

    match result {
        Ok(()) => Ok(()),
        Err(WorktreeFetchLockError::Fetch(error)) if is_marked_transient_fetch_error(&error) => {
            Err(origin_unavailable_refusal(error))
        }
        Err(WorktreeFetchLockError::Fetch(error)) => Err(error),
        Err(WorktreeFetchLockError::Lock(error))
            if is_git_fetch_lock_timeout(&error)
                && origin_probe_confirms_transport_failure(repo_root, branch) =>
        {
            Err(origin_unavailable_refusal(error))
        }
        Err(WorktreeFetchLockError::Lock(error)) => Err(error.into()),
    }
}

enum WorktreeFetchLockError {
    Lock(io::Error),
    Fetch(OrbitError),
}

impl From<io::Error> for WorktreeFetchLockError {
    fn from(error: io::Error) -> Self {
        Self::Lock(error)
    }
}

fn is_git_fetch_lock_timeout(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::TimedOut
        && error
            .get_ref()
            .and_then(|source| source.downcast_ref::<FileLockTimeout>())
            .is_some_and(|timeout| timeout.label == "git fetch")
}

fn origin_probe_confirms_transport_failure(repo_root: &Path, branch: &str) -> bool {
    let mut budget = GitTimeoutBudget::current();
    let probe_timeout_ms = budget
        .fetch_ms
        .min(budget.default_ms)
        .min(ORIGIN_AVAILABILITY_PROBE_TIMEOUT_MS);
    if probe_timeout_ms < ORIGIN_AVAILABILITY_PROBE_MIN_TIMEOUT_MS {
        return false;
    }
    budget.default_ms = probe_timeout_ms;
    budget.fetch_ms = probe_timeout_ms;
    let _budget = GitTimeoutBudgetGuard::install(budget);
    let ref_name = format!("refs/heads/{branch}");
    match git_run(repo_root, &["ls-remote", "--heads", "origin", &ref_name]) {
        Ok(outcome) if outcome.timed_out => true,
        Ok(outcome) if outcome.success => false,
        Ok(outcome) => is_git_transport_failure(&outcome.stderr),
        Err(_) => false,
    }
}

fn is_marked_transient_fetch_error(error: &OrbitError) -> bool {
    match error {
        OrbitError::Execution(message) | OrbitError::ExecutionTimeout { message, .. } => {
            message.contains(TRANSIENT_FAILURE_MARKER)
        }
        _ => false,
    }
}

fn origin_unavailable_refusal(error: impl std::fmt::Display) -> OrbitError {
    OrbitError::DeliveryRefused {
        refusal: orbit_types::workflow::DeliveryRefusal::OriginUnavailable,
        message: format!("origin transport was unavailable during worktree setup: {error}"),
    }
}

pub(in crate::executor::automation) fn normalize_base_branch(
    base: &str,
) -> Result<String, OrbitError> {
    let branch = base
        .trim()
        .strip_prefix("origin/")
        .unwrap_or_else(|| base.trim())
        .trim();
    if branch.is_empty() {
        return Err(OrbitError::InvalidInput(
            "base branch must be a non-empty branch name".to_string(),
        ));
    }
    if branch.starts_with('-') {
        return Err(OrbitError::InvalidInput(format!(
            "base branch '{base}' must not start with '-'"
        )));
    }
    Ok(branch.to_string())
}

fn resolve_local_base_ref(repo_root: &Path, branch: &str) -> Result<String, OrbitError> {
    // Fully qualified: an unqualified name resolves to a same-named tag
    // before the branch, so the returned ref must name the branch exactly.
    let local_base = format!("refs/heads/{branch}");
    if git_command_success(
        repo_root,
        &["rev-parse", "--verify", &format!("{local_base}^{{commit}}")],
    )? {
        return Ok(local_base);
    }

    Err(OrbitError::Execution(format!(
        "unable to resolve local base ref '{branch}' for task worktree creation"
    )))
}

fn resolve_remote_base_ref(repo_root: &Path, branch: &str) -> Result<String, OrbitError> {
    // Fully qualified: the short `origin/<branch>` can resolve to a tag
    // named `origin/<branch>` before the remote-tracking branch.
    let remote_base = format!("refs/remotes/origin/{branch}");
    if git_command_success(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{remote_base}^{{commit}}"),
        ],
    )? {
        return Ok(remote_base);
    }

    Err(OrbitError::Execution(format!(
        "unable to resolve fetched remote base ref '{remote_base}' for task worktree creation"
    )))
}
