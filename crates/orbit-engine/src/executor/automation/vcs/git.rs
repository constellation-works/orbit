use std::cell::Cell;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, GIT_FETCH_LOCK_HOLD_LIMIT, GIT_REMOTE_TIMEOUT,
    git_fetch_cas_retry_delay, should_retry_git_ref_cas, with_git_fetch_lock,
};
use orbit_common::security::child_env::AGENT_SUBPROCESS_BASELINE_VARS;
use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process, run_process_streaming_stdout,
};
use orbit_types::workflow::TRANSIENT_FAILURE_MARKER;
use serde_json::Value;

/// Bounded wall-clock budgets for host Git children.
///
/// Heavyweight mutations get their own defaults; every request still carries a
/// finite `ExecRequest.timeout_ms`. Activity input may overlay these values
/// through `git_timeout_ms` / `git_timeouts` without changing hook or
/// environment policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GitTimeoutBudget {
    pub default_ms: u64,
    pub fetch_ms: u64,
    pub worktree_add_ms: u64,
    pub rebase_ms: u64,
}

impl GitTimeoutBudget {
    pub const MIN_MS: u64 = 1;
    pub const MAX_MS: u64 = 600_000;

    pub const DEFAULT: Self = Self {
        default_ms: 30_000,
        fetch_ms: 60_000,
        worktree_add_ms: 120_000,
        rebase_ms: 120_000,
    };

    pub(crate) fn from_input(input: &Value) -> Result<Self, OrbitError> {
        let mut budget = Self::DEFAULT;
        if let Some(value) = input.get("git_timeout_ms") {
            let ms = parse_timeout_ms(value, "git_timeout_ms")?;
            budget = Self {
                default_ms: ms,
                fetch_ms: ms,
                worktree_add_ms: ms,
                rebase_ms: ms,
            };
        }
        if let Some(timeouts) = input.get("git_timeouts") {
            let Some(map) = timeouts.as_object() else {
                return Err(OrbitError::InvalidInput(
                    "input.git_timeouts must be an object".to_string(),
                ));
            };
            for (key, value) in map {
                let field = format!("git_timeouts.{key}");
                let ms = parse_timeout_ms(value, &field)?;
                match key.as_str() {
                    "default" => budget.default_ms = ms,
                    "fetch" => budget.fetch_ms = ms,
                    "worktree_add" => budget.worktree_add_ms = ms,
                    "rebase" => budget.rebase_ms = ms,
                    other => {
                        return Err(OrbitError::InvalidInput(format!(
                            "unknown git_timeouts key '{other}'; expected default, fetch, worktree_add, or rebase"
                        )));
                    }
                }
            }
        }
        Ok(budget)
    }

    pub(crate) fn current() -> Self {
        GIT_TIMEOUT_BUDGET.with(Cell::get)
    }

    pub(crate) fn timeout_for(self, args: &[&str]) -> u64 {
        // Git's global `-c key=value` may precede the operation (notably
        // `-c core.editor=true rebase --continue`).
        let mut operation = args;
        while let ["-c", _, rest @ ..] = operation {
            operation = rest;
        }
        match operation {
            ["fetch", ..] => self.fetch_ms,
            ["worktree", "add", ..] => self.worktree_add_ms,
            ["rebase", ..] => self.rebase_ms,
            _ => self.default_ms,
        }
    }
}

thread_local! {
    static GIT_TIMEOUT_BUDGET: Cell<GitTimeoutBudget> =
        const { Cell::new(GitTimeoutBudget::DEFAULT) };
}

/// Restores the previous Git timeout budget when the scope exits.
pub(crate) struct GitTimeoutBudgetGuard {
    previous: GitTimeoutBudget,
}

impl GitTimeoutBudgetGuard {
    pub(crate) fn install(budget: GitTimeoutBudget) -> Self {
        let previous = GIT_TIMEOUT_BUDGET.with(|cell| cell.replace(budget));
        Self { previous }
    }
}

impl Drop for GitTimeoutBudgetGuard {
    fn drop(&mut self) {
        GIT_TIMEOUT_BUDGET.with(|cell| cell.set(self.previous));
    }
}

fn parse_timeout_ms(value: &Value, field: &str) -> Result<u64, OrbitError> {
    let Some(ms) = value.as_u64() else {
        return Err(OrbitError::InvalidInput(format!(
            "{field} must be an integer number of milliseconds between {} and {}",
            GitTimeoutBudget::MIN_MS,
            GitTimeoutBudget::MAX_MS
        )));
    };
    if !(GitTimeoutBudget::MIN_MS..=GitTimeoutBudget::MAX_MS).contains(&ms) {
        return Err(OrbitError::InvalidInput(format!(
            "{field} must be between {} and {} ms, got {ms}",
            GitTimeoutBudget::MIN_MS,
            GitTimeoutBudget::MAX_MS
        )));
    }
    Ok(ms)
}

/// Captured Git child result, including timeout vs ordinary failure.
#[derive(Debug, Clone)]
pub(crate) struct GitOutcome {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub timed_out: bool,
    pub timeout_ms: u64,
}

/// Raw stdout for Git commands whose output is a fingerprint input. The
/// timeout verdict is structural; stderr remains diagnostic text only.
pub(crate) struct GitBytesOutcome {
    pub stdout: Vec<u8>,
    pub stderr: String,
    pub success: bool,
    pub timed_out: bool,
    pub timeout_ms: u64,
    pub exit_code: Option<i32>,
}

pub(crate) fn git_timeout_error(
    current_dir: &Path,
    args: &[&str],
    timeout_ms: u64,
    stderr: &str,
) -> OrbitError {
    timeout_recovery_error(
        timeout_ms,
        format!(
            "git {} timed out after {timeout_ms}ms in '{}': {}",
            args.join(" "),
            current_dir.display(),
            stderr.trim()
        ),
    )
}

/// An execution failure caused by a Git deadline, worded by the caller. The
/// timeout class travels in the variant, so recovery guidance never has to
/// recognize it from the text.
pub(crate) fn timeout_recovery_error(timeout_ms: u64, message: String) -> OrbitError {
    OrbitError::ExecutionTimeout {
        timeout_ms,
        message,
    }
}

pub(crate) fn git_failure_error(current_dir: &Path, args: &[&str], stderr: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "git {} failed in '{}': {}",
        args.join(" "),
        current_dir.display(),
        stderr.trim()
    ))
}

/// Run Git under the current budget. Callers that recover from timeout must
/// inspect [`GitOutcome::timed_out`] instead of treating it as a Git exit.
/// That flag is the supervisor's deadline verdict, not a match against
/// captured stderr.
pub(crate) fn git_run(current_dir: &Path, args: &[&str]) -> Result<GitOutcome, OrbitError> {
    let timeout_ms = GitTimeoutBudget::current().timeout_for(args);
    let result = run_process(&git_request(current_dir, args, timeout_ms), &NoSandbox)?;
    Ok(GitOutcome {
        stdout: result.stdout,
        stderr: result.stderr,
        success: result.success,
        timed_out: result.timed_out,
        timeout_ms,
    })
}

/// Supervise host Git with the same secured environment and process-group
/// cleanup as `git_run`, retaining stdout bytes and optional stdin bytes.
pub(crate) fn git_run_bytes(
    current_dir: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<GitBytesOutcome, OrbitError> {
    let timeout_ms = GitTimeoutBudget::current().timeout_for(args);
    let mut request = git_request(current_dir, args, timeout_ms);
    if let Some(bytes) = stdin {
        request.stdin_mode = StdinMode::Bytes(bytes.to_vec());
    }
    let (result, stdout) = run_process_streaming_stdout(&request, &NoSandbox, |mut pipe| {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes)
            .map_err(|error| OrbitError::Execution(format!("read Git stdout: {error}")))?;
        Ok(bytes)
    })?;
    Ok(GitBytesOutcome {
        stdout,
        stderr: result.stderr,
        success: result.success,
        timed_out: result.timed_out,
        timeout_ms,
        exit_code: result.exit_code,
    })
}

/// Compose host VCS environments by name. Git needs the SSH agent socket for
/// authenticated remotes, but never the provider's credentials or ORBIT_* envelope.
/// GitHub CLI callers explicitly supply their own authentication names.
pub(super) fn vcs_environment(extras: &[&str]) -> Vec<(String, String)> {
    AGENT_SUBPROCESS_BASELINE_VARS
        .iter()
        .copied()
        .chain(extras.iter().copied())
        .filter_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| (name.to_string(), value))
        })
        .collect()
}

fn git_environment() -> Vec<(String, String)> {
    let mut environment = vcs_environment(&["SSH_AUTH_SOCK"]);
    environment.push(("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string()));
    environment
}

fn git_args(args: &[&str]) -> Vec<String> {
    // Command-line configuration wins over tracked hooksPath configuration.
    // Disable automatic maintenance too: it may launch additional Git children.
    let mut secured = vec![
        "-c".to_string(),
        "core.hooksPath=/dev/null".to_string(),
        "-c".to_string(),
        "gc.auto=0".to_string(),
    ];
    if let Some((command, rest)) = args.split_first()
        && matches!(*command, "commit" | "push")
    {
        secured.extend([(*command).to_string(), "--no-verify".to_string()]);
        secured.extend(rest.iter().map(|arg| (*arg).to_string()));
    } else {
        secured.extend(args.iter().map(|arg| (*arg).to_string()));
    }
    secured
}

/// The host Git policy shared by deterministic delivery and workspace recovery.
pub(crate) fn git_request(current_dir: &Path, args: &[&str], timeout_ms: u64) -> ExecRequest {
    ExecRequest {
        program: "git".to_string(),
        args: git_args(args),
        current_dir: Some(current_dir.to_string_lossy().into_owned()),
        timeout_ms: Some(timeout_ms),
        stdin_mode: StdinMode::Null,
        environment_mode: EnvironmentMode::ClearAndSet(git_environment()),
        debug: false,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::executor::automation) enum BaseSyncMode {
    Local,
    Remote,
}

pub(in crate::executor::automation) fn base_sync_mode_from_input(
    input: &Value,
) -> Result<BaseSyncMode, OrbitError> {
    match input
        .as_object()
        .and_then(|map| map.get("base_sync"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None | Some("remote") => Ok(BaseSyncMode::Remote),
        Some("local") => Ok(BaseSyncMode::Local),
        Some(other) => Err(OrbitError::InvalidInput(format!(
            "input.base_sync must be 'local' or 'remote', got '{other}'"
        ))),
    }
}

pub(crate) fn git_output_paths(
    current_dir: &Path,
    args: &[&str],
) -> Result<Vec<String>, OrbitError> {
    let raw = git_output_raw(current_dir, args)?;
    Ok(raw
        .split('\0')
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// Runs Git and trims the *entire* stdout string as one unit.
///
/// Safe for single-value output (`rev-parse`, `remote get-url`,
/// `symbolic-ref`, and the like). Do not use this for `git status
/// --porcelain` or any other fixed-width, column-oriented format: trimming
/// the whole string eats the leading status column whenever the result is a
/// single line (` M path` becomes `M path`), misaligning every column-offset
/// read by one byte. Column-offset porcelain readers must use
/// [`git_output_raw`] instead, which preserves each line's leading bytes.
pub(crate) fn git_output(current_dir: &Path, args: &[&str]) -> Result<String, OrbitError> {
    Ok(git_output_raw(current_dir, args)?.trim().to_string())
}

pub(crate) fn git_output_raw(current_dir: &Path, args: &[&str]) -> Result<String, OrbitError> {
    let outcome = git_run(current_dir, args)?;
    if outcome.timed_out {
        return Err(git_timeout_error(
            current_dir,
            args,
            outcome.timeout_ms,
            &outcome.stderr,
        ));
    }
    if !outcome.success {
        return Err(git_failure_error(current_dir, args, &outcome.stderr));
    }

    Ok(outcome.stdout)
}

pub(crate) fn git_success(current_dir: &Path, args: &[&str]) -> Result<(), OrbitError> {
    git_output_raw(current_dir, args).map(|_| ())
}

pub(crate) fn git_command_success(current_dir: &Path, args: &[&str]) -> Result<bool, OrbitError> {
    let outcome = git_run(current_dir, args)?;
    if outcome.timed_out {
        return Err(git_timeout_error(
            current_dir,
            args,
            outcome.timeout_ms,
            &outcome.stderr,
        ));
    }
    Ok(outcome.success)
}

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
                git_failure_error(repo_root, &["fetch", "origin", &spec], &outcome.stderr)
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
            std::thread::sleep(backoff.min(hold_limit.saturating_sub(held_since.elapsed())));
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
            std::thread::sleep(git_fetch_cas_retry_delay());
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
    if git_command_success(
        repo_root,
        &["rev-parse", "--verify", &format!("{branch}^{{commit}}")],
    )? {
        return Ok(branch.to_string());
    }

    Err(OrbitError::Execution(format!(
        "unable to resolve local base ref '{branch}' for task worktree creation"
    )))
}

fn resolve_remote_base_ref(repo_root: &Path, branch: &str) -> Result<String, OrbitError> {
    let remote_base = format!("origin/{branch}");
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
