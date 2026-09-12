use std::cell::Cell;
use std::path::Path;
use std::process::Command;

use orbit_common::OrbitError;
use orbit_common::fs::git::{
    GIT_FETCH_CAS_ATTEMPTS, git_fetch_cas_retry_delay, should_retry_git_ref_cas,
    with_git_fetch_lock,
};
use orbit_common::security::child_env::AGENT_SUBPROCESS_BASELINE_VARS;
use orbit_exec::{EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process};
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
        match args {
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

pub(crate) fn git_timeout_error(
    current_dir: &Path,
    args: &[&str],
    timeout_ms: u64,
    stderr: &str,
) -> OrbitError {
    OrbitError::Execution(format!(
        "git {} timed out after {timeout_ms}ms in '{}': {}",
        args.join(" "),
        current_dir.display(),
        stderr.trim()
    ))
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

/// Byte-preserving adapter for filesystem snapshots and recovery operations.
pub(crate) fn git_command(current_dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(current_dir)
        .args(git_args(args))
        .env_clear()
        .envs(git_environment())
        .stdin(std::process::Stdio::null());
    command
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
pub fn fetch_remote_base(repo_root: &Path, base: &str) -> Result<(), OrbitError> {
    let branch = normalize_base_branch(base)?;
    with_git_fetch_lock(repo_root, || fetch_remote_base_locked(repo_root, &branch))
}

fn fetch_remote_base_locked(repo_root: &Path, branch: &str) -> Result<(), OrbitError> {
    let spec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    let mut last_stderr = String::new();
    for attempt in 0..GIT_FETCH_CAS_ATTEMPTS {
        let outcome = git_run(repo_root, &["fetch", "origin", &spec])?;
        if outcome.timed_out {
            return Err(git_timeout_error(
                repo_root,
                &["fetch", "origin", &spec],
                outcome.timeout_ms,
                &outcome.stderr,
            ));
        }
        if outcome.success {
            return Ok(());
        }
        last_stderr = outcome.stderr.trim().to_string();
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
