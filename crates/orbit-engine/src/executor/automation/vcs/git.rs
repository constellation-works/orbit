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

pub(crate) fn git_output(current_dir: &Path, args: &[&str]) -> Result<String, OrbitError> {
    Ok(git_output_raw(current_dir, args)?.trim().to_string())
}

pub(crate) fn git_output_raw(current_dir: &Path, args: &[&str]) -> Result<String, OrbitError> {
    let result = run_process(&git_request(current_dir, args, 30_000), &NoSandbox)?;

    if !result.success {
        return Err(OrbitError::Execution(format!(
            "git {} failed in '{}': {}",
            args.join(" "),
            current_dir.display(),
            result.stderr.trim()
        )));
    }

    Ok(result.stdout)
}

pub(crate) fn git_success(current_dir: &Path, args: &[&str]) -> Result<(), OrbitError> {
    git_output_raw(current_dir, args).map(|_| ())
}

pub(crate) fn git_command_success(current_dir: &Path, args: &[&str]) -> Result<bool, OrbitError> {
    let result = run_process(&git_request(current_dir, args, 30_000), &NoSandbox)?;
    Ok(result.success)
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
        let result = run_process(
            &git_request(repo_root, &["fetch", "origin", &spec], 60_000),
            &NoSandbox,
        )?;
        if result.success {
            return Ok(());
        }
        last_stderr = result.stderr.trim().to_string();
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
