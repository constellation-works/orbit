//! `Embedder` implementation that talks to the installed companion binary
//! over JSON-Lines stdio. The subprocess is kept alive across requests via
//! a `Mutex<ChildIo>`; a dedicated reader thread feeds stdout into a
//! channel so each RPC waits with a payload-scaled deadline. `Drop` sends
//! `Exit`, closes stdin, and reaps the child within a bounded wait
//! (killing the process group if it ignores `exit`).
//!
//! ## Retry hygiene (ORB-10006)
//!
//! Transport-level failures — spawn resource exhaustion, a crashed/exited
//! companion (EOF), broken pipes, a wedged companion that misses its read
//! deadline — are transient: the request is retried a bounded number of
//! times with exponential backoff + full jitter, respawning the companion
//! between attempts. Companion-reported RPC errors and protocol violations
//! are permanent and surface immediately.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::process::jitter::JitterRng;

use crate::companion::locate_companion;
use crate::embedder::{DEFAULT_MODEL, Embedder};
use crate::rpc::{RpcRequest, RpcResponse, RpcResult, rpc_error_to_orbit};

/// Total request attempts (first try + respawn retries).
pub(crate) const RPC_MAX_ATTEMPTS: u32 = 3;
/// Base of the exponential backoff bound between attempts.
const RPC_RETRY_INITIAL_BACKOFF_MS: u64 = 50;
/// Cap on the backoff bound.
const RPC_RETRY_BACKOFF_CAP_MS: u64 = 1_000;

/// Default per-request wait for a companion response. Large payloads add
/// [`RPC_TIMEOUT_PER_KIB`] so a batch embed is not killed while ONNX runs.
pub(crate) const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(30);
const RPC_TIMEOUT_PER_KIB: Duration = Duration::from_millis(25);

/// How long Drop waits for a cooperative `exit` before killing the child.
pub(crate) const DEFAULT_DROP_TIMEOUT: Duration = Duration::from_secs(2);

const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompanionStderr {
    Inherit,
    Suppress,
}

/// Classification of a single RPC attempt failure (ORB-10006).
pub(crate) enum RequestFailure {
    /// The companion is gone or the pipe broke — a respawn may fix it.
    Transient(String),
    /// Deterministic failure (companion-reported error, protocol violation,
    /// serialization) — retrying cannot fix it.
    Permanent(OrbitError),
}

/// Whether a spawn `io::Error` is deterministic. `NotFound` / rejected
/// permissions won't change between retry attempts; resource exhaustion
/// (EAGAIN, ENOMEM, EMFILE, ...) and anything unrecognized stay transient.
pub(crate) fn spawn_error_is_permanent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}

/// Deterministic exponential bound for the jittered sleep before retry
/// `attempt` (1-based): `min(cap, initial * 2^(attempt-1))`.
pub(crate) fn retry_backoff_bound_ms(attempt: u32) -> u64 {
    RPC_RETRY_INITIAL_BACKOFF_MS
        .saturating_mul(1u64 << attempt.saturating_sub(1).min(20))
        .min(RPC_RETRY_BACKOFF_CAP_MS)
}

/// Per-request stdout wait: `base` plus 25ms for every KiB of the serialized
/// request so large embed batches keep a proportional ONNX budget.
pub(crate) fn rpc_read_deadline(base: Duration, request_line: &str) -> Duration {
    let kib = u32::try_from(request_line.len() / 1024).unwrap_or(u32::MAX);
    base.saturating_add(RPC_TIMEOUT_PER_KIB.saturating_mul(kib))
}

pub struct SubprocessEmbedder {
    model_id: String,
    dim: usize,
    max_input_tokens: usize,
    next_id: AtomicU64,
    io: Mutex<ChildIo>,
    /// Respawn context: the companion path/model/stderr the child was
    /// started with, reused when a transport failure forces a respawn.
    companion_path: PathBuf,
    model_arg: String,
    stderr_mode: CompanionStderr,
    rpc_timeout: Duration,
    drop_timeout: Duration,
}

struct ChildIo {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<std::io::Result<String>>,
    reader: Option<JoinHandle<()>>,
}

impl SubprocessEmbedder {
    pub fn new() -> Result<Self, OrbitError> {
        Self::with_model(DEFAULT_MODEL)
    }

    pub fn with_model(model: &str) -> Result<Self, OrbitError> {
        Self::with_path_and_model(locate_companion()?, model)
    }

    pub fn with_path_and_model(path: PathBuf, model: &str) -> Result<Self, OrbitError> {
        Self::with_path_model_and_stderr(path, model, CompanionStderr::Inherit)
    }

    pub(crate) fn quiet_with_model(model: &str) -> Result<Self, OrbitError> {
        Self::with_path_model_and_stderr(locate_companion()?, model, CompanionStderr::Suppress)
    }

    fn with_path_model_and_stderr(
        path: PathBuf,
        model: &str,
        stderr: CompanionStderr,
    ) -> Result<Self, OrbitError> {
        Self::with_path_model_stderr_and_timeouts(
            path,
            model,
            stderr,
            DEFAULT_RPC_TIMEOUT,
            DEFAULT_DROP_TIMEOUT,
        )
    }

    pub(crate) fn with_path_model_stderr_and_timeouts(
        path: PathBuf,
        model: &str,
        stderr: CompanionStderr,
        rpc_timeout: Duration,
        drop_timeout: Duration,
    ) -> Result<Self, OrbitError> {
        let io = spawn_companion_with_retry(&path, model, stderr)?;
        let mut embedder = Self {
            model_id: String::new(),
            dim: 0,
            max_input_tokens: 0,
            next_id: AtomicU64::new(1),
            io: Mutex::new(io),
            companion_path: path,
            model_arg: model.to_string(),
            stderr_mode: stderr,
            rpc_timeout,
            drop_timeout,
        };
        let info = embedder.request(RpcRequest::Info { id: 0 })?;
        let RpcResult::Info {
            model_id,
            dim,
            max_input_tokens,
            ..
        } = info
        else {
            return Err(OrbitError::AgentProtocolViolation(
                "companion returned non-info response to info request".to_string(),
            ));
        };
        embedder.model_id = model_id;
        embedder.dim = dim;
        embedder.max_input_tokens = max_input_tokens;
        Ok(embedder)
    }

    #[cfg(all(test, unix))]
    pub(crate) fn child_id(&self) -> Option<u32> {
        self.io.lock().ok().map(|io| io.child.id())
    }

    fn request(&self, request: RpcRequest) -> Result<RpcResult, OrbitError> {
        let request = match request {
            RpcRequest::Info { id: 0 } => RpcRequest::Info { id: 1 },
            RpcRequest::Info { .. } => RpcRequest::Info {
                id: self.next_request_id(),
            },
            RpcRequest::Embed { texts, .. } => RpcRequest::Embed {
                id: self.next_request_id(),
                texts,
            },
            RpcRequest::TokenCount { text, .. } => RpcRequest::TokenCount {
                id: self.next_request_id(),
                text,
            },
            RpcRequest::TokenBoundaries { text, .. } => RpcRequest::TokenBoundaries {
                id: self.next_request_id(),
                text,
            },
            RpcRequest::Exit { .. } => RpcRequest::Exit {
                id: self.next_request_id(),
            },
        };
        let line = serde_json::to_string(&request)
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        let id = request.id();
        let deadline = rpc_read_deadline(self.rpc_timeout, &line);

        let mut io = self
            .io
            .lock()
            .map_err(|error| OrbitError::Execution(format!("companion mutex poisoned: {error}")))?;
        let mut jitter = JitterRng::seeded(&self.model_arg);
        let mut last_transient = String::new();
        for attempt in 0..RPC_MAX_ATTEMPTS {
            if attempt > 0 {
                let sleep_ms = jitter.full_jitter(retry_backoff_bound_ms(attempt));
                thread::sleep(Duration::from_millis(sleep_ms));
                match spawn_companion_child(&self.companion_path, &self.model_arg, self.stderr_mode)
                {
                    Ok(fresh) => {
                        io.kill_and_reap();
                        *io = fresh;
                    }
                    Err(error) => {
                        if spawn_error_is_permanent(&error) {
                            return Err(spawn_error_to_orbit(&self.companion_path, &error));
                        }
                        last_transient = format!("companion respawn failed: {error}");
                        tracing::warn!(
                            attempt,
                            error = %error,
                            "search companion respawn failed; will retry"
                        );
                        continue;
                    }
                }
            }
            match request_once(&mut io, &line, id, deadline) {
                Ok(result) => return Ok(result),
                Err(RequestFailure::Permanent(error)) => return Err(error),
                Err(RequestFailure::Transient(message)) => {
                    tracing::warn!(
                        attempt,
                        error = %message,
                        "search companion RPC transport failure; respawning companion"
                    );
                    last_transient = message;
                }
            }
        }
        Err(OrbitError::Execution(format!(
            "search companion RPC failed after {RPC_MAX_ATTEMPTS} attempts: {last_transient}"
        )))
    }

    fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
}

/// One request/response round-trip against the current companion child.
/// Transport failures (write/read errors, EOF, deadline) are transient;
/// malformed or mismatched responses and companion-reported errors are
/// permanent.
fn request_once(
    io: &mut ChildIo,
    line: &str,
    id: u64,
    deadline: Duration,
) -> Result<RpcResult, RequestFailure> {
    let stdin = io
        .stdin
        .as_mut()
        .ok_or_else(|| RequestFailure::Transient("search companion stdin is closed".to_string()))?;
    stdin
        .write_all(line.as_bytes())
        .and_then(|_| stdin.write_all(b"\n"))
        .and_then(|_| stdin.flush())
        .map_err(|error| {
            RequestFailure::Transient(format!("failed to write companion RPC: {error}"))
        })?;

    let response_line = match io.lines.recv_timeout(deadline) {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            io.kill_and_reap();
            return Err(RequestFailure::Transient(format!(
                "failed to read companion RPC: {error}"
            )));
        }
        Err(RecvTimeoutError::Timeout) => {
            tracing::warn!(
                ?deadline,
                "search companion RPC timed out; killing companion"
            );
            io.kill_and_reap();
            return Err(RequestFailure::Transient(format!(
                "search companion RPC timed out after {}ms",
                deadline.as_millis()
            )));
        }
        Err(RecvTimeoutError::Disconnected) => {
            io.kill_and_reap();
            return Err(RequestFailure::Transient(
                "search companion exited before sending a response".to_string(),
            ));
        }
    };
    let response: RpcResponse = serde_json::from_str(&response_line).map_err(|error| {
        RequestFailure::Permanent(OrbitError::AgentProtocolViolation(error.to_string()))
    })?;
    match response {
        RpcResponse::Result {
            id: response_id,
            result,
        } if response_id == id => Ok(result),
        RpcResponse::Error {
            id: response_id,
            error,
        } if response_id == id => Err(RequestFailure::Permanent(rpc_error_to_orbit(error))),
        other => Err(RequestFailure::Permanent(
            OrbitError::AgentProtocolViolation(format!(
                "companion response id mismatch for request {id}: {other:?}"
            )),
        )),
    }
}

impl ChildIo {
    fn kill_and_reap(&mut self) {
        self.stdin.take();
        kill_child_tree(&mut self.child);
        self.join_reader();
    }

    fn join_reader(&mut self) {
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
    }

    fn shutdown_cooperatively(&mut self, drop_timeout: Duration) {
        if let Some(mut stdin) = self.stdin.take() {
            if let Ok(line) = serde_json::to_string(&RpcRequest::Exit { id: 9_999_999 }) {
                let _ = stdin.write_all(line.as_bytes());
                let _ = stdin.write_all(b"\n");
                let _ = stdin.flush();
            }
            drop(stdin);
        }
        if wait_child_until(&mut self.child, drop_timeout) {
            self.join_reader();
            return;
        }
        self.kill_and_reap();
    }
}

fn kill_child_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id();
        // Safety: `killpg` is async-signal-safe. The child was spawned with
        // `process_group(0)`, so its PGID equals its PID and the signal stays
        // inside that group.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn wait_child_until(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(WAIT_POLL_INTERVAL.min(remaining));
            }
            Err(_) => return false,
        }
    }
}

/// Spawn the companion child, retrying transient spawn failures (resource
/// exhaustion) with jittered backoff. Deterministic failures — binary
/// missing, permission denied — surface immediately.
fn spawn_companion_with_retry(
    path: &Path,
    model: &str,
    stderr: CompanionStderr,
) -> Result<ChildIo, OrbitError> {
    let mut jitter = JitterRng::seeded(model);
    let mut last_error: Option<std::io::Error> = None;
    for attempt in 0..RPC_MAX_ATTEMPTS {
        if attempt > 0 {
            let sleep_ms = jitter.full_jitter(retry_backoff_bound_ms(attempt));
            thread::sleep(Duration::from_millis(sleep_ms));
        }
        match spawn_companion_child(path, model, stderr) {
            Ok(io) => return Ok(io),
            Err(error) => {
                if spawn_error_is_permanent(&error) {
                    return Err(spawn_error_to_orbit(path, &error));
                }
                tracing::warn!(
                    attempt,
                    error = %error,
                    "transient search companion spawn failure; will retry"
                );
                last_error = Some(error);
            }
        }
    }
    match last_error {
        Some(error) => Err(spawn_error_to_orbit(path, &error)),
        None => Err(OrbitError::Execution(
            "failed to spawn search companion".to_string(),
        )),
    }
}

fn spawn_error_to_orbit(path: &Path, error: &std::io::Error) -> OrbitError {
    OrbitError::Execution(format!(
        "failed to spawn search companion '{}': {error}",
        path.display()
    ))
}

/// Single spawn attempt; returns the raw `io::Error` so callers can classify.
fn spawn_companion_child(
    path: &Path,
    model: &str,
    stderr: CompanionStderr,
) -> Result<ChildIo, std::io::Error> {
    let mut command = Command::new(path);
    command
        .arg("--model")
        .arg(model)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr.stdio());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("companion stdin unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("companion stdout unavailable"))?;
    match spawn_stdout_reader(stdout) {
        Ok((lines, reader)) => Ok(ChildIo {
            child,
            stdin: Some(stdin),
            lines,
            reader: Some(reader),
        }),
        Err(error) => {
            kill_child_tree(&mut child);
            Err(error)
        }
    }
}

fn spawn_stdout_reader(
    stdout: ChildStdout,
) -> Result<(Receiver<std::io::Result<String>>, JoinHandle<()>), std::io::Error> {
    let (tx, rx) = mpsc::channel();
    let handle = thread::Builder::new()
        .name("orbit-search-companion-stdout".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                        break;
                    }
                }
            }
        })?;
    Ok((rx, handle))
}

impl CompanionStderr {
    fn stdio(self) -> Stdio {
        match self {
            Self::Inherit => Stdio::inherit(),
            Self::Suppress => Stdio::null(),
        }
    }
}

impl Embedder for SubprocessEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn max_input_tokens(&self) -> usize {
        self.max_input_tokens
    }

    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, OrbitError> {
        let result = self.request(RpcRequest::Embed {
            id: 0,
            texts: texts.iter().map(|text| (*text).to_string()).collect(),
        })?;
        match result {
            RpcResult::Embed { vectors } => Ok(vectors),
            _ => Err(OrbitError::AgentProtocolViolation(
                "companion returned non-embed response to embed request".to_string(),
            )),
        }
    }

    fn token_count(&self, text: &str) -> Result<usize, OrbitError> {
        let result = self.request(RpcRequest::TokenCount {
            id: 0,
            text: text.to_string(),
        })?;
        match result {
            RpcResult::TokenCount { tokens } => Ok(tokens),
            _ => Err(OrbitError::AgentProtocolViolation(
                "companion returned non-token_count response".to_string(),
            )),
        }
    }

    fn token_boundaries(&self, text: &str) -> Result<Vec<usize>, OrbitError> {
        let result = self.request(RpcRequest::TokenBoundaries {
            id: 0,
            text: text.to_string(),
        })?;
        match result {
            RpcResult::TokenBoundaries { ends } => Ok(ends),
            _ => Err(OrbitError::AgentProtocolViolation(
                "companion returned non-token_boundaries response".to_string(),
            )),
        }
    }
}

impl Drop for SubprocessEmbedder {
    fn drop(&mut self) {
        let mut io = match self.io.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        io.shutdown_cooperatively(self.drop_timeout);
    }
}
