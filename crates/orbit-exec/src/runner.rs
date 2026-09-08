use std::process::{Child, ChildStdout};
use std::thread;
use std::time::Instant;

use orbit_common::OrbitError;
use orbit_common::security::redaction::is_sensitive_env_name;
use orbit_types::tool::ExecutionResult;

use crate::sandbox::Sandbox;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StdinMode {
    #[default]
    Inherit,
    Null,
    Bytes(Vec<u8>),
}

#[derive(Clone, PartialEq, Eq, Default)]
pub enum EnvironmentMode {
    #[default]
    Inherit,
    ClearAndSet(Vec<(String, String)>),
}

impl std::fmt::Debug for EnvironmentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inherit => write!(f, "Inherit"),
            Self::ClearAndSet(pairs) => {
                let redacted: Vec<(&str, &str)> = pairs
                    .iter()
                    .map(|(k, v)| {
                        if is_sensitive_env_name(k) {
                            (k.as_str(), "[REDACTED]")
                        } else {
                            (k.as_str(), v.as_str())
                        }
                    })
                    .collect();
                f.debug_tuple("ClearAndSet").field(&redacted).finish()
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecRequest {
    pub program: String,
    pub args: Vec<String>,
    pub current_dir: Option<String>,
    pub timeout_ms: Option<u64>,
    pub stdin_mode: StdinMode,
    pub environment_mode: EnvironmentMode,
    /// When `true`, tee redaction-aware child stdout/stderr to the terminal
    /// while preserving captured stdout for downstream parsing.
    pub debug: bool,
}

/// Run a process after applying the supplied Orbit sandbox strategy.
///
/// The strategy can add validation or containment before spawning the child.
/// Passing [`NoSandbox`](crate::NoSandbox) applies no additional Orbit sandbox,
/// but does not disable or escape any sandbox already imposed on this process.
/// In particular, children inherit macOS Seatbelt restrictions and Linux
/// descendants inherit the Bubblewrap mount namespace of an outer provider
/// sandbox.
pub fn run_process(
    req: &ExecRequest,
    sandbox: &dyn Sandbox,
) -> Result<ExecutionResult, OrbitError> {
    sandbox.validate(req)?;

    let started = Instant::now();
    let child = sandbox.spawn(req)?;
    let stdin_payload = match &req.stdin_mode {
        StdinMode::Bytes(bytes) => Some(bytes.clone()),
        StdinMode::Inherit | StdinMode::Null => None,
    };
    let result = crate::supervision::wait_with_optional_timeout(
        child,
        req.timeout_ms,
        req.debug,
        stdin_payload,
    )?;

    Ok(ExecutionResult {
        success: result.exit_success,
        timed_out: result.timed_out,
        stdout: String::from_utf8_lossy(&result.stdout).to_string(),
        stderr: String::from_utf8_lossy(&result.stderr).to_string(),
        exit_code: result.exit_code,
        duration_ms: started.elapsed().as_millis() as u64,
        output: None,
    })
}

/// Outcome of supervising a child Orbit did not spawn itself.
///
/// The deadline verdict is retained separately for callers that need it
/// alongside the complete process result.
#[derive(Debug, Clone)]
pub struct SupervisedOutcome {
    pub result: ExecutionResult,
    /// The wall-clock deadline elapsed and the supervisor terminated the
    /// child's process group.
    pub timed_out: bool,
}

/// Supervise a child that the caller already spawned.
///
/// [`run_process`] is the entry point when Orbit creates the child itself.
/// Callers that must build the child through a sandbox wrapper — the
/// `spawn_under_linux_bwrap` / `spawn_under_macos_sandbox` helpers in this
/// crate — hand the spawned child here so output draining, the wall-clock
/// deadline, signal-driven cancellation, and process-group cleanup stay
/// byte-identical to the unsandboxed path instead of being reimplemented per
/// call site.
///
/// `stdin_payload` is written to the child's stdin pipe and then closed.
/// Passing `Some(Vec::new())` closes stdin immediately, which is what a
/// non-interactive step wants: a piped-but-never-closed stdin leaves a reader
/// blocked until the deadline.
pub fn supervise_child(
    child: Child,
    timeout_ms: Option<u64>,
    stdin_payload: Option<Vec<u8>>,
) -> Result<SupervisedOutcome, OrbitError> {
    let started = Instant::now();
    let result =
        crate::supervision::wait_with_optional_timeout(child, timeout_ms, false, stdin_payload)?;
    Ok(SupervisedOutcome {
        result: ExecutionResult {
            success: result.exit_success,
            timed_out: result.timed_out,
            stdout: String::from_utf8_lossy(&result.stdout).to_string(),
            stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            exit_code: result.exit_code,
            duration_ms: started.elapsed().as_millis() as u64,
            output: None,
        },
        timed_out: result.timed_out,
    })
}

/// Run a process while consuming stdout incrementally instead of retaining it.
///
/// Callers that need to inspect a potentially large stream should keep their
/// own accumulator bounded in `consume`. Stderr remains subject to Orbit's
/// normal output-capture limit, and the returned `stdout` is intentionally
/// empty because the stream was handed to the consumer.
pub fn run_process_streaming_stdout<T, F>(
    req: &ExecRequest,
    sandbox: &dyn Sandbox,
    consume: F,
) -> Result<(ExecutionResult, T), OrbitError>
where
    T: Send + 'static,
    F: FnOnce(ChildStdout) -> Result<T, OrbitError> + Send + 'static,
{
    sandbox.validate(req)?;

    let started = Instant::now();
    let mut child = sandbox.spawn(req)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| OrbitError::Execution("process stdout was not piped".to_string()))?;
    let stdout_thread = thread::spawn(move || consume(stdout));
    let stdin_payload = match &req.stdin_mode {
        StdinMode::Bytes(bytes) => Some(bytes.clone()),
        StdinMode::Inherit | StdinMode::Null => None,
    };
    // `stdout` was deliberately removed above. The standard supervisor still
    // drains stderr, enforces timeouts, and cleans up the child process group.
    let result = crate::supervision::wait_with_optional_timeout(
        child,
        req.timeout_ms,
        req.debug,
        stdin_payload,
    )?;
    let consumed = stdout_thread
        .join()
        .map_err(|_| OrbitError::Execution("stdout consumer thread panicked".to_string()))??;

    Ok((
        ExecutionResult {
            success: result.exit_success,
            timed_out: result.timed_out,
            stdout: String::new(),
            stderr: String::from_utf8_lossy(&result.stderr).to_string(),
            exit_code: result.exit_code,
            duration_ms: started.elapsed().as_millis() as u64,
            output: None,
        },
        consumed,
    ))
}
