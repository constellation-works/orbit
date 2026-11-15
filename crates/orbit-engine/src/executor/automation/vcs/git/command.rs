use super::timeout::{command_timeout, git_timeout_error};
use orbit_common::OrbitError;
use orbit_common::security::child_env::AGENT_SUBPROCESS_BASELINE_VARS;
use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process, run_process_streaming_stdout,
};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

/// Captured Git child result, including timeout vs ordinary failure.
#[derive(Debug, Clone)]
pub(crate) struct GitOutcome {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub timed_out: bool,
    pub timeout_ms: u64,
    pub exit_code: Option<i32>,
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

pub(crate) fn git_failure_error(
    current_dir: &Path,
    args: &[&str],
    exit_code: Option<i32>,
    stderr: &str,
) -> OrbitError {
    // Signal termination is reported by the process supervisor on stderr.
    // An absent code alone must not be guessed to mean a signal: cancellation
    // and output-capture limits also return no exit code.
    let status = exit_code
        .map(|code| format!("exit status: {code}"))
        .unwrap_or_else(|| "no exit code".to_string());
    OrbitError::Execution(format!(
        "git {} failed in '{}' ({status}): {}",
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
    let timeout_ms = command_timeout(args)?;
    let result = run_process(&git_request(current_dir, args, timeout_ms), &NoSandbox)?;
    Ok(GitOutcome {
        stdout: result.stdout,
        stderr: result.stderr,
        success: result.success,
        timed_out: result.timed_out,
        timeout_ms,
        exit_code: result.exit_code,
    })
}

/// Supervise host Git with the same secured environment and process-group
/// cleanup as `git_run`, retaining stdout bytes and optional stdin bytes.
pub(crate) fn git_run_bytes(
    current_dir: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
) -> Result<GitBytesOutcome, OrbitError> {
    let timeout_ms = command_timeout(args)?;
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
/// GitHub CLI callers take [`gh_environment`], which adds its authentication names.
fn vcs_environment(extras: &[&str]) -> Vec<(String, String)> {
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

/// The one environment every host-side `gh` child runs under: the baseline
/// plus GitHub's authentication, host and config names, never the provider's
/// credentials or the ORBIT_* envelope.
pub(in crate::executor::automation) fn gh_environment() -> Vec<(String, String)> {
    vcs_environment(&[
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
        "GH_HOST",
        "GH_CONFIG_DIR",
        "XDG_CONFIG_HOME",
    ])
}

fn git_environment() -> Vec<(String, String)> {
    let mut environment = vcs_environment(&["SSH_AUTH_SOCK"]);
    // Git stderr drives retry classification. LANGUAGE can override gettext's
    // locale selection, so never admit it and replace the parent's LC_ALL.
    environment.retain(|(name, _)| name != "LC_ALL" && name != "LANGUAGE");
    environment.push(("LC_ALL".to_string(), "C".to_string()));
    environment.push(("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string()));
    environment
}

fn git_args(args: &[&str]) -> Vec<String> {
    // Command-line configuration wins over tracked hooksPath configuration.
    // Disable automatic maintenance too: it may launch additional Git children.
    // Porcelain status and diff never recurse into a gitlink's checkout, whose
    // own repository config (a filter, fsmonitor or hook) an agent could plant.
    let mut secured = vec![
        "-c".to_string(),
        "core.hooksPath=/dev/null".to_string(),
        "-c".to_string(),
        "core.fsmonitor=false".to_string(),
        "-c".to_string(),
        "gc.auto=0".to_string(),
        "-c".to_string(),
        "diff.ignoreSubmodules=all".to_string(),
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

/// How a run resolves its configured base branch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BaseSyncMode {
    /// Read the checkout's local branch.
    Local,
    /// Fetch and read the remote-tracking branch.
    #[default]
    Remote,
}

impl std::str::FromStr for BaseSyncMode {
    type Err = OrbitError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "" | "remote" => Ok(Self::Remote),
            "local" => Ok(Self::Local),
            other => Err(OrbitError::InvalidInput(format!(
                "input.base_sync must be 'local' or 'remote', got '{other}'"
            ))),
        }
    }
}

impl<'de> serde::Deserialize<'de> for BaseSyncMode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <String as serde::Deserialize>::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

impl BaseSyncMode {
    /// Parse the job input's base sync mode. Missing, non-string and empty
    /// values use remote sync, matching the delivery input contract.
    pub fn from_input(input: &Value) -> Result<Self, OrbitError> {
        input
            .get("base_sync")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .parse()
    }
}

pub(in crate::executor::automation) fn base_sync_mode_from_input(
    input: &Value,
) -> Result<BaseSyncMode, OrbitError> {
    BaseSyncMode::from_input(input)
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
        return Err(git_failure_error(
            current_dir,
            args,
            outcome.exit_code,
            &outcome.stderr,
        ));
    }

    Ok(outcome.stdout)
}

/// Run Git with `env` added to its secured environment; its trimmed stdout.
pub(crate) fn git_output_with_env(
    current_dir: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String, OrbitError> {
    let timeout_ms = command_timeout(args)?;
    let mut request = git_request(current_dir, args, timeout_ms);
    if let EnvironmentMode::ClearAndSet(environment) = &mut request.environment_mode {
        environment.extend(
            env.iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string())),
        );
    }
    let result = run_process(&request, &NoSandbox)?;
    if !result.success {
        return Err(git_failure_error(
            current_dir,
            args,
            result.exit_code,
            &result.stderr,
        ));
    }
    Ok(result.stdout.trim().to_string())
}

pub(crate) fn git_success(current_dir: &Path, args: &[&str]) -> Result<(), OrbitError> {
    git_output_raw(current_dir, args).map(|_| ())
}

/// A URL argument that reaches `push_url` itself, without Git applying
/// `url.*.insteadOf` to the already-resolved push URL a second time.
///
/// `git remote get-url --push` resolves `pushInsteadOf` (and, when no
/// push-specific rule matches, `insteadOf`) exactly once, as `git push
/// origin` does. Passing that resolved URL to `ls-remote` or `fetch`
/// would apply `insteadOf` again and can name a different repository
/// [ORB-17686]. The probe URL appends a suffix segment so an ephemeral
/// identity rewrite for it is strictly longer — and therefore wins —
/// over every `insteadOf` rule whose value is a prefix of `push_url`.
/// The identity rewrite maps the probe URL back to `push_url`, so the
/// transport still addresses exactly `push_url`. This adds only a
/// command-line `-c` value; it never writes the user's Git config.
#[derive(Debug, Clone)]
pub(crate) struct DirectRemoteTarget {
    pub probe_url: String,
    pub config_arg: Option<String>,
}

impl DirectRemoteTarget {
    pub(crate) fn prefix_args(&self) -> Vec<String> {
        match &self.config_arg {
            Some(config) => vec!["-c".to_string(), config.clone()],
            None => Vec::new(),
        }
    }
}

pub(crate) fn direct_remote_target(push_url: &str) -> DirectRemoteTarget {
    // A `-c url.<push_url>.insteadOf=...` key cannot carry these bytes:
    // `=` would end the config name early and the rest corrupt parsing.
    // Such URLs fall back to a plain probe, which
    // `verify_direct_remote_target` then refuses if it would rewrite.
    if push_url.is_empty() || push_url.contains(['=', '\n', '\r', '\\', '"']) {
        return DirectRemoteTarget {
            probe_url: push_url.to_string(),
            config_arg: None,
        };
    }
    let separator = if push_url.ends_with('/') { "" } else { "/" };
    let probe_url = format!("{push_url}{separator}.orbit-publication-probe");
    DirectRemoteTarget {
        config_arg: Some(format!("url.{push_url}.insteadOf={probe_url}")),
        probe_url,
    }
}

/// Fail closed when Git would still rewrite `target` away from
/// `push_url` (for example a rule matching the probe URL itself at the
/// same length). Probing or fetching the rewritten repository could
/// report publication at a destination `git push origin` never wrote.
pub(crate) fn verify_direct_remote_target(
    current_dir: &Path,
    push_url: &str,
    target: &DirectRemoteTarget,
) -> Result<(), OrbitError> {
    let mut args: Vec<&str> = Vec::new();
    if let Some(config) = target.config_arg.as_deref() {
        args.extend(["-c", config]);
    }
    args.extend(["ls-remote", "--get-url", target.probe_url.as_str()]);
    let expanded = git_output(current_dir, &args)?;
    if expanded != push_url {
        return Err(OrbitError::Execution(format!(
            "git_push: effective push URL '{push_url}' would be rewritten to '{expanded}' \
             when probed or fetched by URL, so it does not name the repository \
             'git push origin' writes; refusing to probe or fetch a different repository"
        )));
    }
    Ok(())
}

pub(crate) fn is_ancestor(
    dir: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, OrbitError> {
    git_command_success(
        dir,
        &[
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            ancestor,
            descendant,
        ],
    )
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
