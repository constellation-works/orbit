//! The `mcp` backend: one stdio MCP server per plugin per *caller context* —
//! the workspace the call is made from and the allowed-tools intersection —
//! per runtime process, spawned on first use under the plugin's sandbox,
//! verified against the manifest, and proxied for every `<ns>.<verb>` call
//! (design §4.2).
//!
//! Orbit is the only client. Each child is kept for the life of this
//! [`McpBackend`], which the runtime holds for its own lifetime; a crashed
//! or unresponsive child ends the current call with an error, is killed, and
//! is respawned by the next call with the same key. A caller whose context
//! differs from a live session's gets its own child rather than inheriting
//! another caller's workspace or `ORBIT_ALLOWED_TOOLS`. There is no code path
//! that waits without a deadline.
//!
//! One process serves several workspaces (`orbit clock tick`, `orbit mcp
//! serve`), and a child is bound to a workspace three times over: its working
//! directory, its `ORBIT_WORKSPACE_ROOT`, and the sandbox profile's rendered
//! `{{workspace}}` write roots. So the workspace is part of the session key,
//! and the per-call context the `exec` envelope carries travels on
//! `tools/call` instead of only in the child's environment.
//!
//! Only the map of sessions is behind the backend's own lock; each session
//! has its own, so one caller's slow call does not hold up another session.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_exec::{EnvironmentMode, ExecRequest, Sandbox, StdinMode};
use serde_json::{Value, json};

use super::backend::PluginBackendSpec;
use super::callback::PluginCallbackSession;
use super::envelope::{CallSecrets, apply_secret_updates, call_context, plugin_error};
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
    startups: Mutex<BTreeMap<SessionKey, Arc<Mutex<()>>>>,
}

enum McpState {
    Ready {
        /// Live children keyed by the caller context they were spawned for.
        /// Callers that share a key share a child; a different key never
        /// reuses one.
        sessions: BTreeMap<SessionKey, Arc<McpSessionHandle>>,
    },
    /// The server disagreed with the manifest; nothing restarts it.
    Refused(String),
}

/// What a child is bound to, and therefore what two callers must agree on
/// before they may share one. The workspace decides the child's working
/// directory, its `ORBIT_WORKSPACE_ROOT` and the write roots its sandbox
/// profile renders from `{{workspace}}`, so a session keyed by the
/// allowed-tools intersection alone would proxy workspace B's call to a
/// child confined to workspace A [ORB-12820].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SessionKey {
    /// The child's working directory: the caller's workspace root when it
    /// has one, else its cwd.
    cwd: String,
    /// The caller's workspace root, which is not always its cwd and is what
    /// the sandbox profile is rendered from.
    workspace_root: Option<String>,
    /// The sorted `permissions.orbit_tools` ∩ grant ∩ `ctx.allowed_tools`
    /// intersection. Order in the caller's own list must not split sessions
    /// that carry the same tools.
    allowed_tools: Vec<String>,
}

/// One live child, plus the pid a supervisor or a test can read *while* a
/// call holds the session. The pid is fixed for the session's life, so
/// reading it never waits on the call in flight.
struct McpSessionHandle {
    pid: u32,
    session: Mutex<McpSession>,
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: i64,
    /// Host-issued callback identity for this child. Dropped when the
    /// session ends so ancestry no longer treats the pid as a plugin.
    callback: Option<PluginCallbackSession>,
    cancelled: Option<Arc<std::sync::atomic::AtomicBool>>,
    ended: bool,
}

impl McpBackend {
    pub fn new(spec: Arc<PluginBackendSpec>, expected: Vec<McpExpectedTool>) -> Self {
        Self {
            spec,
            expected,
            startups: Mutex::new(BTreeMap::new()),
            state: Mutex::new(McpState::Ready {
                sessions: BTreeMap::new(),
            }),
        }
    }

    pub(crate) fn for_broker(&self) -> Self {
        Self::new(Arc::clone(&self.spec), self.expected.clone())
    }

    pub fn spec(&self) -> &Arc<PluginBackendSpec> {
        &self.spec
    }

    /// The pid of the server serving `ctx`, for a supervisor or a test that
    /// kills it. There is one child per caller context, so the question only
    /// has an answer once a context is named. Answers during that context's
    /// call, which holds the session but not the pid.
    pub fn child_pid(&self, ctx: &ToolContext) -> Option<u32> {
        let key = self.session_key(ctx, "").ok()?;
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            McpState::Ready { sessions } => sessions.get(&key).map(|handle| handle.pid),
            McpState::Refused(_) => None,
        }
    }

    /// Whether any server started by this runtime is still alive.
    pub fn is_running(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &*state {
            McpState::Ready { sessions } => sessions.values().any(|handle| handle.is_live()),
            McpState::Refused(_) => false,
        }
    }

    /// The server disagreed with the manifest: nothing restarts it, and the
    /// sessions are dropped. A session another caller is mid-call on ends
    /// when that call releases its last reference, rather than having its
    /// wire cut underneath it.
    fn refuse(&self, state: &mut McpState, diagnostic: String) {
        *state = McpState::Refused(diagnostic);
    }

    /// The caller context this call's child must be bound to.
    fn session_key(&self, ctx: &ToolContext, tool_name: &str) -> Result<SessionKey, OrbitError> {
        let mut allowed_tools = self.spec.allowed_tools(ctx);
        allowed_tools.sort();
        allowed_tools.dedup();
        Ok(SessionKey {
            cwd: child_cwd(ctx, tool_name)?,
            workspace_root: ctx
                .workspace_root
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            allowed_tools,
        })
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
        let deadline = Instant::now() + timeout;
        let key = self.session_key(ctx, tool_name)?;
        // A shared child cannot be told in its environment which caller the
        // call is for, so the context an `exec` backend reads from its stdin
        // envelope rides the request (§4.2). So do the plugin's secrets, read
        // for this call: a value set since the child started reaches it
        // without a respawn, and none is ever in its environment.
        let secrets = CallSecrets::resolve(&self.spec)?;
        let params = tools_call_params(&self.spec, ctx, tool_name, verb, input, &secrets);
        // At most one retry: the session this call found may have been ended
        // by another caller's failure between the lookup and the lock, and
        // that caller's broken wire is not this one's error.
        let mut retried = false;
        loop {
            let handle = self.ensure_running(&key, ctx, tool_name, deadline)?;
            let outcome = {
                let mut session = lock_for_call(&handle.session, ctx, deadline)?;
                session.cancelled = ctx
                    .broker_call
                    .as_ref()
                    .map(|call| Arc::clone(&call.cancelled));
                if !session.is_live() && !retried {
                    retried = true;
                    drop(session);
                    self.retire(&key, &handle);
                    continue;
                }
                secrets.record_delivery();
                session.request("tools/call", params.clone(), deadline)
            };
            return match outcome {
                Ok(response) => {
                    // A rotation rides the result's `_meta.orbit`, the reply
                    // to the request's `_meta.orbit.secrets`, and is applied
                    // whether or not the call itself succeeded.
                    apply_secret_updates(
                        &self.spec,
                        tool_name,
                        response.pointer("/result/_meta/orbit/secret_updates"),
                    );
                    tool_result(tool_name, &response)
                }
                Err(error) => {
                    // Whatever happened, the wire is no longer in a known
                    // state: an unanswered request would otherwise be matched
                    // by a later call's id. Kill this context's child and let
                    // the next matching call respawn it.
                    self.retire(&key, &handle);
                    Err(OrbitError::Execution(format!(
                        "plugin tool '{tool_name}': the plugin's mcp server {error}; it will be \
                         restarted on the next call"
                    )))
                }
            };
        }
    }

    /// The session for `key`, spawning and verifying one when there is none.
    ///
    /// The map lock is taken only to look a session up and to publish one:
    /// the handshake happens on a session no other caller can reach yet, so
    /// a slow start no more blocks another workspace's call than a slow call
    /// does.
    fn ensure_running(
        &self,
        key: &SessionKey,
        ctx: &ToolContext,
        tool_name: &str,
        deadline: Instant,
    ) -> Result<Arc<McpSessionHandle>, OrbitError> {
        if let Some(handle) = self.live_session(key)? {
            return Ok(handle);
        }
        // Serialize startup only for this key. Concurrent first calls must
        // not spawn duplicate children (and duplicate startup side effects).
        let startup = {
            let mut startups = self.startups.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(startups.entry(key.clone()).or_default())
        };
        let _startup = lock_for_call(&startup, ctx, deadline)?;
        if let Some(handle) = self.live_session(key)? {
            return Ok(handle);
        }
        let mut session = self.spawn(ctx, &key.cwd)?;
        let pid = session.child.id();
        if let Err(error) = session.handshake(deadline) {
            return Err(OrbitError::Execution(format!(
                "plugin tool '{tool_name}': the plugin's mcp server {error}"
            )));
        }
        let advertised = session.list_tools(deadline).map_err(|error| {
            OrbitError::Execution(format!(
                "plugin tool '{tool_name}': the plugin's mcp server {error}"
            ))
        })?;
        let mismatch = manifest_mismatch(&self.expected, &advertised);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(mismatch) = mismatch {
            let diagnostic = format!(
                "plugin '{}' refused to start: its mcp server's tools/list disagrees with the \
                 manifest: {mismatch}",
                self.spec.provenance.name
            );
            self.refuse(&mut state, diagnostic.clone());
            return Err(OrbitError::Execution(diagnostic));
        }
        let sessions = match &mut *state {
            // Refused while this one was handshaking: another session of the
            // same plugin disagreed with the manifest, and nothing restarts
            // any of them. This child is dropped — and killed — unpublished.
            McpState::Refused(diagnostic) => {
                return Err(OrbitError::Execution(diagnostic.clone()));
            }
            McpState::Ready { sessions } => sessions,
        };
        let handle = Arc::new(McpSessionHandle {
            pid,
            session: Mutex::new(session),
        });
        sessions.insert(key.clone(), Arc::clone(&handle));
        Ok(handle)
    }

    /// The published session for `key` when it is still alive, reaping a
    /// child that exited on its own between calls.
    fn live_session(&self, key: &SessionKey) -> Result<Option<Arc<McpSessionHandle>>, OrbitError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &mut *state {
            McpState::Refused(diagnostic) => Err(OrbitError::Execution(diagnostic.clone())),
            McpState::Ready { sessions } => match sessions.get(key) {
                Some(handle) if handle.is_live() => Ok(Some(Arc::clone(handle))),
                Some(_) => {
                    sessions.remove(key);
                    Ok(None)
                }
                None => Ok(None),
            },
        }
    }

    /// End one session and unpublish it, so the next call with this key
    /// spawns a fresh child. The entry is removed only while it is still
    /// *this* session: another caller may already have published a
    /// replacement under the same key.
    fn retire(&self, key: &SessionKey, handle: &Arc<McpSessionHandle>) {
        {
            let mut session = handle
                .session
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            session.end();
        }
        // The session lock is released first: `live_session` holds the map
        // lock while it looks at a session, so taking them in the other
        // order here would be the inversion that deadlocks.
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let McpState::Ready { sessions } = &mut *state
            && sessions
                .get(key)
                .is_some_and(|live| Arc::ptr_eq(live, handle))
        {
            sessions.remove(key);
        }
    }

    /// Spawn one confined child for `cwd`, the working directory the
    /// session key resolved for this caller's context.
    fn spawn(&self, ctx: &ToolContext, cwd: &str) -> Result<McpSession, OrbitError> {
        let cwd = cwd.to_string();
        let mut environment = self.spec.child_environment(ctx, &cwd, None);
        // The same intersection the session key carries, recorded as the
        // child's callback ceiling: a session is shared only by callers who
        // agree on it, so one ceiling describes every caller it serves
        // [ORB-12801].
        let mut callback = PluginCallbackSession::mint(
            &self.spec.global_root,
            &self.spec.provenance,
            &self.spec.allowed_tools(ctx),
        )?;
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
        let profile = match &ctx.brokered_caller {
            Some(caller) => self.spec.brokered_sandbox_profile(caller)?,
            None => self.spec.sandbox_profile(ctx.workspace_root.as_deref())?,
        };
        let sandbox = profile.with_callback_session(&callback);
        sandbox.validate(&request)?;
        let mut child = sandbox.spawn(&request)?;
        if let Err(error) = callback.bind_pid(child.id()) {
            kill_group(&mut child);
            return Err(error);
        }
        let mut session = McpSession::start(child)?;
        session.callback = Some(callback);
        session.cancelled = ctx
            .broker_call
            .as_ref()
            .map(|call| Arc::clone(&call.cancelled));
        Ok(session)
    }
}

impl McpSessionHandle {
    /// Whether this session's child is still running, without ever waiting
    /// on the call that may be holding it: a session another caller is
    /// mid-request on is by construction alive, and blocking here would put
    /// every lookup behind the slowest call again.
    fn is_live(&self) -> bool {
        match self.session.try_lock() {
            Ok(mut session) => session.is_live(),
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().is_live(),
        }
    }
}

/// The working directory the child is spawned in: the caller's workspace
/// root when it has one, else its cwd.
fn child_cwd(ctx: &ToolContext, tool_name: &str) -> Result<String, OrbitError> {
    ctx.workspace_root
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| ctx.cwd.clone())
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin tool '{tool_name}' requires ToolContext.cwd"
            ))
        })
}

/// The `tools/call` params one proxied call sends.
///
/// `_meta.orbit` is the `context` object an `exec` backend reads from its
/// stdin envelope (`envelope.rs`), plus the tool name: one `mcp` child serves
/// every tool of its plugin and every caller sharing its key, so
/// `ORBIT_TOOL_NAME` is absent from its environment (§4.2) and
/// `ORBIT_WORKSPACE_ROOT` names the workspace the session is bound to rather
/// than this call's. The plugin's declared secrets ride here too, as
/// `_meta.orbit.secrets`.
pub(crate) fn tools_call_params(
    spec: &PluginBackendSpec,
    ctx: &ToolContext,
    tool_name: &str,
    verb: &str,
    input: Value,
    secrets: &CallSecrets,
) -> Value {
    json!({
        "name": verb,
        "arguments": input,
        "_meta": { "orbit": call_context(spec, ctx, Some(tool_name), secrets) },
    })
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
pub(crate) fn tool_result(tool_name: &str, response: &Value) -> Result<Value, OrbitError> {
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
        let text_error = text
            .as_deref()
            .and_then(|text| serde_json::from_str::<Value>(text).ok());
        if let Some(error) = structured
            .or(text_error.as_ref())
            .and_then(|value| plugin_error(tool_name, value))
        {
            return Err(error);
        }
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
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            kill_group(&mut child);
            return Err(OrbitError::Execution(
                "mcp server has no stdio pipes".to_string(),
            ));
        };
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: stdin owns this live descriptor. Nonblocking writes let
            // cancellation and deadlines interrupt a server that stops reading.
            let flags = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL) };
            if flags < 0
                || unsafe {
                    libc::fcntl(stdin.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
                } < 0
            {
                kill_group(&mut child);
                return Err(OrbitError::Execution(
                    "cannot make MCP stdin nonblocking".to_string(),
                ));
            }
        }
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
            cancelled: None,
            ended: false,
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
        self.send(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            deadline,
        )
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
        self.send(
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
            deadline,
        )?;
        loop {
            self.check_deadline(deadline)?;
            let remaining = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50));
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    continue;
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
            let message_id = message.get("id").filter(|value| !value.is_null());
            // A message that names a method is the server calling *us*,
            // whatever its id: each peer numbers its own requests, so a
            // server ping may carry the very id this request is waiting on.
            // A request of its own carries an id that must be answered; a
            // notification is skipped.
            if let Some(server_method) = message.get("method").and_then(Value::as_str) {
                if let Some(server_id) = message_id.cloned() {
                    self.answer(&server_id, server_method, deadline)?;
                }
                continue;
            }
            if message_id.and_then(Value::as_i64) != Some(id) {
                // A stale response to an earlier request.
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
            if message.get("result").is_none() {
                return Err(format!(
                    "answered '{method}' with neither a result nor an error"
                ));
            }
            return Ok(message);
        }
    }

    /// Answer one server-initiated request. A client that never answers is
    /// not a quiet client: a server that pings (or asks for `roots/list`)
    /// before finishing the `tools/call` it is answering waits for a reply
    /// that never comes, and the call dies at the deadline with the child
    /// killed as unresponsive [ORB-12820]. Orbit declares no capabilities in
    /// `initialize`, so `ping` — which every MCP client owes — is the one
    /// method it serves and the rest are method-not-found.
    fn answer(&mut self, id: &Value, method: &str, deadline: Instant) -> Result<(), String> {
        let response = match method {
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {
                    "code": -32601,
                    "message": format!("orbit's plugin host does not serve '{method}'"),
                },
            }),
        };
        self.send(&response, deadline)
    }

    fn send(&mut self, message: &Value, deadline: Instant) -> Result<(), String> {
        let mut line = serde_json::to_string(message)
            .map_err(|error| format!("could not be sent a request: {error}"))?;
        line.push('\n');
        let mut bytes = line.as_bytes();
        while !bytes.is_empty() {
            self.check_deadline(deadline)?;
            match self.stdin.write(bytes) {
                Ok(0) => return Err("closed its stdin".to_string()),
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("closed its stdin: {error}")),
            }
        }
        Ok(())
    }

    fn check_deadline(&self, deadline: Instant) -> Result<(), String> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err("call was cancelled".to_string());
        }
        if Instant::now() >= deadline {
            return Err("did not answer within the timeout".to_string());
        }
        Ok(())
    }

    /// Reap a dead session and reclaim its descendants on the first observation.
    fn is_live(&mut self) -> bool {
        if self.ended {
            return false;
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            return true;
        }
        // Reclaim descendants immediately, even if another waiter retains
        // this handle. Drop must never signal this reaped PID a second time.
        self.end();
        false
    }

    /// End the callback identity, kill the process group and reap the child.
    /// Idempotent, because Drop runs it again.
    fn end(&mut self) {
        if !self.ended {
            self.ended = true;
            self.callback.take();
            kill_group(&mut self.child);
        }
    }
}

/// Unpublishing a session is what ends it: the last holder — the map, or the
/// call that was still using it — drops the child here, so no path leaves a
/// server running past the backend that spawned it.
impl Drop for McpSession {
    fn drop(&mut self) {
        self.end();
    }
}

/// All sandbox spawn paths create a process group with the child's PID.
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    // SAFETY: this session owns the child group created by the sandbox.
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn lock_for_call<'a, T>(
    mutex: &'a Mutex<T>,
    ctx: &ToolContext,
    deadline: Instant,
) -> Result<std::sync::MutexGuard<'a, T>, OrbitError> {
    loop {
        if ctx
            .broker_call
            .as_ref()
            .is_some_and(|call| call.cancelled.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(OrbitError::Execution(
                "plugin call cancelled while waiting for its session".to_string(),
            ));
        }
        if Instant::now() >= deadline {
            return Err(OrbitError::Execution(
                "plugin session remained busy until the call timeout".to_string(),
            ));
        }
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}
