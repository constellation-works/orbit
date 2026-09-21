//! The `mcp` backend: one stdio MCP server per plugin per allowed-tools
//! intersection per runtime process, spawned on first use under the plugin's
//! sandbox, verified against the manifest, and proxied for every
//! `<ns>.<verb>` call (design §4.2).
//!
//! Orbit is the only client. Each child is kept for the life of this
//! [`McpBackend`], which the runtime holds for its own lifetime; a crashed
//! or unresponsive child ends the current call with an error, is killed, and
//! is respawned by the next call with the same intersection. A caller whose
//! intersection differs from a live session's gets its own child rather than
//! inheriting another caller's `ORBIT_ALLOWED_TOOLS`. There is no code path
//! that waits without a deadline.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, Sandbox, StdinMode};
use serde_json::{Value, json};

use super::backend::PluginBackendSpec;
use super::callback::PluginCallbackSession;
use crate::ToolContext;

/// Lines the reader may queue ahead of the consumer before it blocks.
const LINE_QUEUE: usize = 64;
/// Longest single line accepted from the server; every MCP message is one
/// JSON line, and anything past this is not one.
const MAX_LINE_BYTES: u64 = 64 * 1024 * 1024;
/// The MCP revision Orbit's own server speaks, and the one a plugin server
/// must accept.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// One manifest tool as the server must advertise it.
#[derive(Debug, Clone, PartialEq)]
pub struct McpExpectedTool {
    pub verb: String,
    /// The manifest's resolved `input_schema`, when it declared one. A tool
    /// without one accepts whatever the server advertises.
    pub input_schema: Option<Value>,
}

pub struct McpBackend {
    spec: Arc<PluginBackendSpec>,
    expected: Vec<McpExpectedTool>,
    state: Mutex<McpState>,
    /// The last-used child's pid, readable while a call holds `state`; zero
    /// when that child is gone.
    pid: AtomicU32,
}

enum McpState {
    Ready {
        /// Live children keyed by the sorted `allowed_tools` intersection the
        /// session was spawned with. Callers with the same intersection share
        /// a child; a different intersection never reuses one.
        sessions: BTreeMap<Vec<String>, McpSession>,
    },
    /// The server disagreed with the manifest; nothing restarts it.
    Refused(String),
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: i64,
    /// Host-issued callback identity for this child. Dropped when the
    /// session ends so ancestry no longer treats the pid as a plugin.
    callback: Option<PluginCallbackSession>,
}

impl McpBackend {
    pub fn new(spec: Arc<PluginBackendSpec>, expected: Vec<McpExpectedTool>) -> Self {
        Self {
            spec,
            expected,
            state: Mutex::new(McpState::Ready {
                sessions: BTreeMap::new(),
            }),
            pid: AtomicU32::new(0),
        }
    }

    pub fn spec(&self) -> &Arc<PluginBackendSpec> {
        &self.spec
    }

    /// The running server's pid, for a supervisor or a test that kills it.
    /// Answers during a call, which holds the session itself.
    pub fn child_pid(&self) -> Option<u32> {
        match self.pid.load(Ordering::Acquire) {
            0 => None,
            pid => Some(pid),
        }
    }

    fn end_session(&self, session: &mut McpSession) {
        let pid = session.child.id();
        session.callback.take();
        session.kill();
        let _ = self
            .pid
            .compare_exchange(pid, 0, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// Whether any server started by this runtime is still alive.
    pub fn is_running(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *state {
            McpState::Ready { sessions } => sessions
                .values_mut()
                .any(|session| matches!(session.child.try_wait(), Ok(None))),
            McpState::Refused(_) => false,
        }
    }

    fn refuse(&self, state: &mut McpState, diagnostic: String) {
        if let McpState::Ready { sessions } = state {
            for session in sessions.values_mut() {
                self.end_session(session);
            }
        }
        *state = McpState::Refused(diagnostic);
    }

    /// Proxy one `<ns>.<verb>` call as `tools/call`.
    pub fn call(
        &self,
        ctx: &ToolContext,
        tool_name: &str,
        verb: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        let timeout = Duration::from_millis(self.spec.timeout_ms());
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        self.ensure_running(&mut state, ctx, tool_name, timeout)?;
        let key = allowed_tools_key(&self.spec.allowed_tools(ctx));
        let McpState::Ready { sessions } = &mut *state else {
            return Err(OrbitError::Execution(format!(
                "plugin tool '{tool_name}': the mcp backend is not running"
            )));
        };
        let outcome = {
            let session = sessions.get_mut(&key).ok_or_else(|| {
                OrbitError::Execution(format!(
                    "plugin tool '{tool_name}': the mcp backend is not running"
                ))
            })?;
            session.request(
                "tools/call",
                json!({ "name": verb, "arguments": input }),
                Instant::now() + timeout,
            )
        };
        match outcome {
            Ok(response) => tool_result(tool_name, &response),
            Err(error) => {
                // Whatever happened, the wire is no longer in a known state:
                // an unanswered request would otherwise be matched by a
                // later call's id. Kill this intersection's child and let
                // the next matching call respawn it.
                if let Some(session) = sessions.get_mut(&key) {
                    self.end_session(session);
                }
                sessions.remove(&key);
                Err(OrbitError::Execution(format!(
                    "plugin tool '{tool_name}': the plugin's mcp server {error}; it will be \
                     restarted on the next call"
                )))
            }
        }
    }

    fn ensure_running(
        &self,
        state: &mut McpState,
        ctx: &ToolContext,
        tool_name: &str,
        timeout: Duration,
    ) -> Result<(), OrbitError> {
        let key = allowed_tools_key(&self.spec.allowed_tools(ctx));
        match state {
            McpState::Refused(diagnostic) => {
                return Err(OrbitError::Execution(diagnostic.clone()));
            }
            McpState::Ready { sessions } => {
                if let Some(session) = sessions.get_mut(&key) {
                    if matches!(session.child.try_wait(), Ok(None)) {
                        self.pid.store(session.child.id(), Ordering::Release);
                        return Ok(());
                    }
                    // Exited on its own between calls: reap and respawn.
                    self.end_session(session);
                }
                sessions.remove(&key);
            }
        }
        let mut session = self.spawn(ctx, tool_name)?;
        self.pid.store(session.child.id(), Ordering::Release);
        let deadline = Instant::now() + timeout;
        if let Err(error) = session.handshake(deadline) {
            self.end_session(&mut session);
            return Err(OrbitError::Execution(format!(
                "plugin tool '{tool_name}': the plugin's mcp server {error}"
            )));
        }
        let advertised = match session.list_tools(deadline) {
            Ok(advertised) => advertised,
            Err(error) => {
                self.end_session(&mut session);
                return Err(OrbitError::Execution(format!(
                    "plugin tool '{tool_name}': the plugin's mcp server {error}"
                )));
            }
        };
        if let Some(mismatch) = manifest_mismatch(&self.expected, &advertised) {
            self.end_session(&mut session);
            let diagnostic = format!(
                "plugin '{}' refused to start: its mcp server's tools/list disagrees with the \
                 manifest: {mismatch}",
                self.spec.provenance.name
            );
            self.refuse(state, diagnostic.clone());
            return Err(OrbitError::Execution(diagnostic));
        }
        match state {
            McpState::Ready { sessions } => {
                sessions.insert(key, session);
            }
            McpState::Refused(diagnostic) => {
                self.end_session(&mut session);
                return Err(OrbitError::Execution(diagnostic.clone()));
            }
        }
        Ok(())
    }

    fn spawn(&self, ctx: &ToolContext, tool_name: &str) -> Result<McpSession, OrbitError> {
        let cwd = ctx
            .workspace_root
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| ctx.cwd.clone())
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "plugin tool '{tool_name}' requires ToolContext.cwd"
                ))
            })?;
        let mut environment = self.spec.child_environment(ctx, &cwd, None);
        let mut callback =
            PluginCallbackSession::mint(&self.spec.global_root, &self.spec.provenance)?;
        callback.stamp_env(&mut environment);
        let request = ExecRequest {
            program: self.spec.command.to_string_lossy().into_owned(),
            args: self.spec.args.clone(),
            current_dir: Some(cwd.clone()),
            timeout_ms: None,
            // A piped stdin the session keeps open for its lifetime.
            stdin_mode: StdinMode::Bytes(Vec::new()),
            environment_mode: EnvironmentMode::ClearAndSet(environment),
            debug: false,
        };
        let sandbox = self.spec.sandbox_profile(ctx.workspace_root.as_deref())?;
        sandbox.validate(&request)?;
        let mut child = sandbox.spawn(&request)?;
        if let Err(error) = callback.bind_pid(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let mut session = McpSession::start(child)?;
        session.callback = Some(callback);
        Ok(session)
    }
}

impl Drop for McpBackend {
    fn drop(&mut self) {
        if let McpState::Ready { sessions } =
            self.state.get_mut().unwrap_or_else(PoisonError::into_inner)
        {
            for session in sessions.values_mut() {
                session.kill();
            }
        }
    }
}

/// Stable map key for one caller's `permissions.orbit_tools` ∩ grant ∩
/// `ctx.allowed_tools` intersection. Order in the caller's own list must not
/// split sessions that carry the same tools.
fn allowed_tools_key(tools: &[String]) -> Vec<String> {
    let mut key = tools.to_vec();
    key.sort();
    key.dedup();
    key
}

/// The first disagreement between the manifest and the server, naming the
/// tool: a manifest tool the server lacks, a server tool the manifest never
/// declared, or an input schema that differs.
fn manifest_mismatch(expected: &[McpExpectedTool], advertised: &[Value]) -> Option<String> {
    for tool in expected {
        let Some(served) = advertised
            .iter()
            .find(|served| served["name"].as_str() == Some(tool.verb.as_str()))
        else {
            return Some(format!(
                "the manifest declares tool '{}' but the server does not serve it",
                tool.verb
            ));
        };
        if let Some(schema) = &tool.input_schema
            && served.get("inputSchema") != Some(schema)
        {
            return Some(format!(
                "tool '{}' has a different input schema on the server than in the manifest",
                tool.verb
            ));
        }
    }
    for served in advertised {
        let name = served["name"].as_str().unwrap_or("<unnamed>");
        if !expected.iter().any(|tool| tool.verb == name) {
            return Some(format!(
                "the server serves tool '{name}' which the manifest does not declare"
            ));
        }
    }
    None
}

/// Map a `tools/call` result onto the envelope's `output`: an `isError`
/// result is the tool error, `structuredContent` is the output when present,
/// otherwise the single text content parsed as JSON, or the raw text.
fn tool_result(tool_name: &str, response: &Value) -> Result<Value, OrbitError> {
    let result = &response["result"];
    let structured = result
        .get("structuredContent")
        .filter(|value| !value.is_null());
    let text = result["content"].as_array().and_then(|items| {
        let texts: Vec<&str> = items
            .iter()
            .filter(|item| item["type"] == "text")
            .filter_map(|item| item["text"].as_str())
            .collect();
        (texts.len() == 1).then(|| texts[0].to_string())
    });
    if result["isError"].as_bool().unwrap_or(false) {
        let message = structured
            .and_then(|value| value["message"].as_str().map(ToOwned::to_owned))
            .or(text)
            .unwrap_or_else(|| "the server reported an error without a message".to_string());
        return Err(OrbitError::Execution(format!(
            "plugin tool '{tool_name}' failed: {message}"
        )));
    }
    if let Some(structured) = structured {
        return Ok(structured.clone());
    }
    Ok(match text {
        Some(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        None => result["content"].clone(),
    })
}

impl McpSession {
    fn start(mut child: Child) -> Result<Self, OrbitError> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| OrbitError::Execution("mcp server has no stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| OrbitError::Execution("mcp server has no stdout".to_string()))?;
        // The reader thread is what makes the deadline real: a blocking read
        // on a wedged server cannot otherwise be abandoned, and the thread
        // ends when the killed child closes the pipe.
        let (sender, lines) = sync_channel(LINE_QUEUE);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.by_ref().take(MAX_LINE_BYTES).read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if !line.ends_with('\n') && line.len() as u64 >= MAX_LINE_BYTES {
                    break;
                }
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        // Stderr is drained so a chatty server cannot block on a full pipe.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut sink = std::io::sink();
                let _ = std::io::copy(&mut BufReader::new(stderr), &mut sink);
            });
        }
        Ok(Self {
            child,
            stdin,
            lines,
            next_id: 0,
            callback: None,
        })
    }

    fn handshake(&mut self, deadline: Instant) -> Result<(), String> {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "orbit-plugin-host", "version": env!("CARGO_PKG_VERSION") },
            }),
            deadline,
        )?;
        if response["result"].get("protocolVersion").is_none() {
            return Err("answered initialize without a protocolVersion".to_string());
        }
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
    }

    fn list_tools(&mut self, deadline: Instant) -> Result<Vec<Value>, String> {
        let response = self.request("tools/list", json!({}), deadline)?;
        response["result"]["tools"]
            .as_array()
            .cloned()
            .ok_or_else(|| "answered tools/list without a tools array".to_string())
    }

    fn request(&mut self, method: &str, params: Value, deadline: Instant) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(format!("did not answer '{method}' within the timeout"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(format!("exited before answering '{method}'"));
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(line.trim())
                .map_err(|error| format!("emitted invalid JSON: {error}"))?;
            if message.get("id").and_then(Value::as_i64) != Some(id) {
                // A notification or another id: not this request's answer.
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error["message"].as_str().unwrap_or("").to_string();
                return Err(format!(
                    "rejected '{method}': {}",
                    if text.is_empty() {
                        error.to_string()
                    } else {
                        text
                    }
                ));
            }
            return Ok(message);
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        let mut line = serde_json::to_string(message)
            .map_err(|error| format!("could not be sent a request: {error}"))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("closed its stdin: {error}"))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
