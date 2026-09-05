//! Deterministic local command execution for v2 job steps [ORB-11294].
//!
//! This is the supported replacement for the deleted v1 `cli_command`
//! executor. A `local_shell` activity runs one child process through the same
//! `orbit-exec` supervision the agent CLI runner uses — process-group spawn,
//! output draining, wall-clock deadline, signal-driven cancellation, orphan
//! cleanup — and reports stdout, stderr, and the exact exit status.
//!
//! Two rules shape the input contract:
//!
//! * **The program and its arguments come from `config` only.** A job step's
//!   `with:` block is rendered from templates and may carry text an agent
//!   produced; letting it reach argv would make every shell step an injection
//!   surface. Runtime input contributes the checkout to run in, nothing else.
//! * **Argv execution and shell execution are different declarations.**
//!   `command` + `args` is an `execve` with no shell anywhere in the chain.
//!   Running `sh -c` requires naming the interpreter in `shell` and the script
//!   in `script`. Orbit never joins `args` into a command line.
//!
//! A shell step is not an agent: it receives no prompt, no model, no tool
//! allowlist, and none of the Orbit registry/workspace identity variables a
//! CLI agent is launched with.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_exec::supervise_child;
use serde_json::{Value, json};

use crate::activity_job::cli_runner::spawn::{
    prepare_sandbox_for_dispatch, spawn_child_with_optional_sandbox,
};
use crate::context::RuntimeHost;

use super::StateExecutionContext;
use super::input::{canonicalize_existing_dir, input_string_field};

/// Executor definition consulted when `config.executor` is absent. It ships
/// with Orbit and is the definition legacy `cli_command` assets already carry.
const DEFAULT_SHELL_EXECUTOR: &str = "local-shell";

/// Wall-clock budget applied when neither the activity config nor the executor
/// definition sets one.
const DEFAULT_TIMEOUT_MS: u64 = 600_000;

/// Upper bound on a single step's budget. A step that legitimately needs more
/// than an hour belongs in its own job, where the run-level controls apply.
const MAX_TIMEOUT_MS: u64 = 3_600_000;

/// Flag used to hand a script to the declared interpreter.
const SHELL_COMMAND_FLAG: &str = "-c";

/// How the child's argv was declared.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Invocation {
    /// `command` + `args`: literal argv, executed directly.
    Argv { command: String, args: Vec<String> },
    /// `shell` + `script`: the named interpreter is invoked as
    /// `<shell> -c <script>`.
    Shell { shell: String, script: String },
}

/// The parts of a `local_shell` step that come from static activity config.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ShellConfig {
    invocation: Option<Invocation>,
    executor: String,
    /// Directory to run in, relative to the resolved workspace root unless
    /// absolute. `None` means the workspace root itself.
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    timeout_ms: Option<u64>,
    allow_nonzero_exit: bool,
}

pub(super) fn local_shell<H: RuntimeHost + Sync + ?Sized>(
    host: &H,
    config: &Value,
    input: &Value,
    state_context: Option<&StateExecutionContext>,
) -> Result<Value, OrbitError> {
    let config = parse_shell_config(config)?;
    let executor = host
        .resolve_local_shell_executor(&config.executor)
        .map_err(|error| {
            OrbitError::InvalidInput(format!(
                "local_shell executor '{}' is unusable: {error}",
                config.executor
            ))
        })?;
    let (program, args) = compose_argv(&config, &executor)?;

    let workspace_root = resolve_workspace_root(host, input)?;
    let cwd = resolve_cwd(&workspace_root, config.cwd.as_deref())?;

    let mut environment = host.agent_subprocess_environment(&[]);
    apply_env_overrides(&mut environment, &executor.env);
    apply_env_overrides(&mut environment, &config.env);

    let timeout_ms = resolve_timeout_ms(&config, executor.timeout_seconds)?;

    // The activity's own fsProfile decides what the child may touch, exactly as
    // it does for a CLI agent. A definition that declares no sandbox runs bare —
    // that is the shipped `local-shell` posture, and it is reported in the
    // step's output rather than left implicit.
    let sandbox = host
        .resolve_executor_sandbox(
            &config.executor,
            state_context.and_then(|context| context.fs_profile.as_deref()),
            Some(&cwd),
        )
        .map_err(|error| {
            OrbitError::Execution(format!(
                "resolve sandbox for local_shell executor '{}': {error}",
                config.executor
            ))
        })?;
    let prepared = prepare_sandbox_for_dispatch(sandbox.as_ref())
        .map_err(|error| OrbitError::Execution(error.to_string()))?;

    let spawned = spawn_child_with_optional_sandbox(
        &program,
        &args,
        &environment,
        Some(&cwd),
        prepared.effective,
        &config.executor,
    )
    .map_err(|error| OrbitError::Execution(error.to_string()))?;

    // An empty stdin payload closes the pipe immediately. A shell step is
    // non-interactive, and a child left reading an open-but-silent stdin would
    // only ever end at the deadline.
    let outcome = supervise_child(spawned.child, Some(timeout_ms), Some(Vec::new()))?;
    let result = outcome.result;

    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program.clone());
    argv.extend(args.iter().cloned());
    let output = json!({
        "success": result.success,
        "exit_code": result.exit_code,
        "stdout": result.stdout,
        "stderr": result.stderr,
        "duration_ms": result.duration_ms,
        "timed_out": outcome.timed_out,
        "timeout_ms": timeout_ms,
        "argv": argv,
        "cwd": cwd.display().to_string(),
        "sandbox": prepared.metadata.backend.clone().unwrap_or_else(|| "none".to_string()),
    });

    if result.success || config.allow_nonzero_exit {
        return Ok(output);
    }
    Err(OrbitError::Execution(failure_message(
        &program,
        &cwd,
        result.exit_code,
        outcome.timed_out,
        timeout_ms,
        &result.stderr,
    )))
}

fn failure_message(
    program: &str,
    cwd: &Path,
    exit_code: Option<i32>,
    timed_out: bool,
    timeout_ms: u64,
    stderr: &str,
) -> String {
    let status = if timed_out {
        format!("timed out after {timeout_ms}ms")
    } else {
        match exit_code {
            Some(code) => format!("exited with status {code}"),
            None => "terminated without an exit status".to_string(),
        }
    };
    let trailer = match stderr.trim() {
        "" => String::new(),
        text => format!(": {text}"),
    };
    format!(
        "local_shell command '{program}' in '{}' {status}{trailer}",
        cwd.display()
    )
}

fn parse_shell_config(config: &Value) -> Result<ShellConfig, OrbitError> {
    let command = config_string(config, "command")?;
    let shell = config_string(config, "shell")?;
    let script = config
        .get("script")
        .map(|value| {
            value.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                OrbitError::InvalidInput("local_shell config.script must be a string".to_string())
            })
        })
        .transpose()?;
    let args = config_string_list(config, "args")?;

    let invocation = match (command, shell) {
        (Some(_), Some(_)) => {
            return Err(OrbitError::InvalidInput(
                "local_shell config sets both `command` and `shell`; declare argv execution or \
                 shell execution, not both"
                    .to_string(),
            ));
        }
        (Some(command), None) => {
            if script.is_some() {
                return Err(OrbitError::InvalidInput(
                    "local_shell config.script requires `shell`; argv execution takes `args`"
                        .to_string(),
                ));
            }
            Some(Invocation::Argv { command, args })
        }
        (None, Some(shell)) => {
            let script = script
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "local_shell config.shell requires a non-empty `script`".to_string(),
                    )
                })?;
            if !args.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "local_shell config.args is not allowed with `shell`; positional arguments \
                     after a `-c` script rebind `$0`, so put them in the script itself"
                        .to_string(),
                ));
            }
            Some(Invocation::Shell { shell, script })
        }
        // No program named here: the executor definition may still supply one,
        // and `compose_argv` decides.
        (None, None) => {
            if script.is_some() {
                return Err(OrbitError::InvalidInput(
                    "local_shell config.script requires `shell`".to_string(),
                ));
            }
            if !args.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "local_shell config.args requires `command`".to_string(),
                ));
            }
            None
        }
    };

    Ok(ShellConfig {
        invocation,
        executor: config_string(config, "executor")?
            .unwrap_or_else(|| DEFAULT_SHELL_EXECUTOR.to_string()),
        cwd: config_string(config, "cwd")?,
        env: config_env(config)?,
        timeout_ms: config_u64(config, "timeout_ms")?,
        allow_nonzero_exit: config
            .get("allow_nonzero_exit")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    OrbitError::InvalidInput(
                        "local_shell config.allow_nonzero_exit must be a boolean".to_string(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(false),
    })
}

/// Merge the executor definition's defaults with the activity's declaration.
///
/// The definition's `command` is the fallback program — this is what a legacy
/// `cli_command` definition's `command` field means under the current runtime —
/// and its `args` are a static prefix, so an operator can pin a wrapper once
/// instead of repeating it in every activity.
fn compose_argv(
    config: &ShellConfig,
    executor: &crate::activity_job::ResolvedShellExecutor,
) -> Result<(String, Vec<String>), OrbitError> {
    match &config.invocation {
        Some(Invocation::Argv { command, args }) => {
            let mut composed = executor.args.clone();
            composed.extend(args.iter().cloned());
            Ok((command.clone(), composed))
        }
        Some(Invocation::Shell { shell, script }) => Ok((
            shell.clone(),
            vec![SHELL_COMMAND_FLAG.to_string(), script.clone()],
        )),
        None => {
            let command = executor.command.clone().ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "local_shell needs a program: set `command` (with optional `args`) or `shell` \
                     plus `script` in the activity config, or give executor '{}' a default \
                     `command`",
                    config.executor
                ))
            })?;
            Ok((command, executor.args.clone()))
        }
    }
}

/// The checkout the step runs against: the run's worktree when the pipeline
/// supplied one, otherwise the registered repository root. Same selector the
/// deterministic VCS actions use, so a shell step and a `git_commit` step in
/// one job always agree on which checkout they are looking at.
fn resolve_workspace_root<H: RuntimeHost + ?Sized>(
    host: &H,
    input: &Value,
) -> Result<PathBuf, OrbitError> {
    match input_string_field(input, "workspace_path") {
        Some(path) => canonicalize_existing_dir(&path, "workspace_path"),
        None => canonicalize_existing_dir(&host.repo_root()?, "repo_root"),
    }
}

/// Resolve `cwd` against the workspace root and refuse to leave it.
///
/// Containment is enforced here, not only by the sandbox: the shipped
/// `local-shell` definition declares no OS sandbox, so without this check a
/// `cwd: ../..` would silently run the step outside the workspace the run is
/// authorized for.
fn resolve_cwd(workspace_root: &Path, cwd: Option<&str>) -> Result<PathBuf, OrbitError> {
    let Some(raw) = cwd.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(workspace_root.to_path_buf());
    };
    let candidate = Path::new(raw);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        workspace_root.join(candidate)
    };
    let resolved = canonicalize_existing_dir(&joined.to_string_lossy(), "cwd")?;
    if !resolved.starts_with(workspace_root) {
        return Err(OrbitError::InvalidInput(format!(
            "local_shell cwd '{raw}' resolves to '{}', which is outside the workspace root '{}'",
            resolved.display(),
            workspace_root.display()
        )));
    }
    Ok(resolved)
}

fn resolve_timeout_ms(
    config: &ShellConfig,
    executor_timeout_seconds: Option<u64>,
) -> Result<u64, OrbitError> {
    let timeout_ms = config
        .timeout_ms
        .or_else(|| executor_timeout_seconds.map(|seconds| seconds.saturating_mul(1_000)))
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    if timeout_ms == 0 || timeout_ms > MAX_TIMEOUT_MS {
        return Err(OrbitError::InvalidInput(format!(
            "local_shell timeout must be between 1 and {MAX_TIMEOUT_MS} ms, got {timeout_ms}"
        )));
    }
    Ok(timeout_ms)
}

/// Layer explicit entries onto the host's resolved `[execution.env]` baseline.
///
/// The baseline is the same credential-free environment an agent subprocess
/// gets; nothing ambient is inherited, and a later layer replaces an earlier
/// entry with the same name rather than appending a duplicate.
fn apply_env_overrides(
    environment: &mut Vec<(String, String)>,
    overrides: &BTreeMap<String, String>,
) {
    for (name, value) in overrides {
        match environment.iter_mut().find(|(key, _)| key == name) {
            Some(entry) => entry.1 = value.clone(),
            None => environment.push((name.clone(), value.clone())),
        }
    }
}

fn config_string(config: &Value, key: &str) -> Result<Option<String>, OrbitError> {
    let Some(value) = config.get(key) else {
        return Ok(None);
    };
    let text = value.as_str().ok_or_else(|| {
        OrbitError::InvalidInput(format!("local_shell config.{key} must be a string"))
    })?;
    Ok(Some(text.trim().to_string()).filter(|value| !value.is_empty()))
}

fn config_string_list(config: &Value, key: &str) -> Result<Vec<String>, OrbitError> {
    let Some(value) = config.get(key) else {
        return Ok(Vec::new());
    };
    let items = value.as_array().ok_or_else(|| {
        OrbitError::InvalidInput(format!("local_shell config.{key} must be an array"))
    })?;
    items
        .iter()
        .map(|item| {
            item.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "local_shell config.{key} must contain only strings"
                ))
            })
        })
        .collect()
}

fn config_env(config: &Value) -> Result<BTreeMap<String, String>, OrbitError> {
    let Some(value) = config.get("env") else {
        return Ok(BTreeMap::new());
    };
    let map = value.as_object().ok_or_else(|| {
        OrbitError::InvalidInput("local_shell config.env must be an object".to_string())
    })?;
    map.iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_string()))
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "local_shell config.env.{name} must be a string"
                    ))
                })
        })
        .collect()
}

fn config_u64(config: &Value, key: &str) -> Result<Option<u64>, OrbitError> {
    let Some(value) = config.get(key) else {
        return Ok(None);
    };
    value.as_u64().map(Some).ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "local_shell config.{key} must be a non-negative integer"
        ))
    })
}

#[cfg(test)]
#[path = "tests/shell.rs"]
mod tests;
