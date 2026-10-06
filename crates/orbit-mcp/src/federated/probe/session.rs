//! Bounded MCP session I/O, request deadlines, and lost-answer classification.

use std::io::{BufRead, BufReader, Write};
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_types::tool::mcp_advertised_tool_name;
use serde_json::{Value, json};

use super::super::config::Destination;
use super::contracts::DestinationSnapshot;
use super::discovery::{remote_tool_error, snapshot_from_discovery_content, unreachable};

/// Lines the probe reader may queue ahead of the consumer before it blocks.
const PROBE_LINE_QUEUE: usize = 64;

/// Longest line the reader accepts while the session is only probing: the
/// `initialize` answer, workspace discovery, and `tools/list`. Those replies are
/// small by construction (the whole advertised tool surface is a few hundred
/// KiB), so a destination that streams megabytes without a newline is not
/// answering, and buffering up to the tool-result ceiling first would let a
/// hostile host pin that much memory per probe.
const MAX_PROBE_LINE_BYTES: u64 = 4 * 1024 * 1024;

/// Longest line the reader accepts once a routed `tools/call` is in flight.
/// Tool results can legitimately be large, so delivery keeps the ceiling the
/// reader had before probes were tightened.
const MAX_TOOL_RESULT_LINE_BYTES: u64 = 64 * 1024 * 1024;

/// How long a write that outlived its budget gets to settle once the session
/// is killed. Killing the child closes the pipe, so the blocked write fails at
/// once; this only bounds a transport whose pipe some other process still
/// holds open.
const WRITE_SETTLE_GRACE: Duration = Duration::from_secs(1);

/// The MCP protocol revision this client negotiates. Pinned to the revision
/// Orbit's own server answers with, so a probe fails loudly on a real protocol
/// change rather than silently degrading.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// How to name a request whose answer never arrived, once its bytes are on
/// the wire.
///
/// Losing the answer is not the same fact for every request. The phases that
/// decide a route are read-only and repeatable, so silence there means the
/// host did not answer. An unreadable answer has the same uncertainty as a
/// missing one. A routed `tools/call` may already have run and
/// committed on the destination, and killing the SSH child does not undo it,
/// so silence there is genuine ambiguity: reporting it as a delivery miss
/// invites the retry that duplicates the write [ORB-11023].
#[derive(Clone, Copy)]
enum LostAnswer<'a> {
    Unreachable,
    OutcomeUnknown { tool: &'a str },
}

impl LostAnswer<'_> {
    fn classify(self, destination: &Destination, request_id: i64, reason: String) -> OrbitError {
        match self {
            Self::Unreachable => unreachable(destination, reason),
            Self::OutcomeUnknown { tool } => OrbitError::OutcomeUnknown {
                // The destination-facing request identity, which is what an
                // operator can correlate against that host's audit log.
                mcp_call_id: format!("{}/{tool}#{request_id}", destination.machine_id),
                message: format!("{reason}; the destination may have completed the call"),
            },
        }
    }
}

/// One MCP client session against a destination, bounded by one deadline at a
/// time.
///
/// The deadline is a budget for the request in flight, not for the session:
/// [`DestinationSession::restart_budget`] re-stamps it when a phase with its
/// own budget begins. It covers writing the request as well as reading the
/// answer.
pub(in crate::federated) struct DestinationSession {
    destination: Destination,
    child: Child,
    /// `None` once a write failed or outlived its budget: the destination may
    /// hold a partial line, so nothing more can be framed after it.
    writer: Option<RequestWriter>,
    lines: Receiver<Result<String, LineTooLong>>,
    /// Ceiling the reader thread applies to the line it is assembling. Raised
    /// before a routed `tools/call` is written, so it is never lower than the
    /// phase in flight even while the reader is already blocked mid-line.
    line_cap: Arc<AtomicU64>,
    deadline: Instant,
    next_id: i64,
    pub(super) worker_invocation: Option<orbit_types::tool::WorkerInvocation>,
}

impl DestinationSession {
    pub(in crate::federated) fn start(
        destination: Destination,
        mut child: Child,
        timeout: Duration,
    ) -> Result<Self, OrbitError> {
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| unreachable(&destination, "SSH session has no stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| unreachable(&destination, "SSH session has no stdout".to_string()))?;
        let writer = RequestWriter::spawn(stdin);
        // A reader thread is what makes the deadline real: a blocking read on
        // an unresponsive host cannot otherwise be abandoned, and the thread
        // ends on its own when the killed child closes the pipe. It is also
        // bounded in both directions: the queue applies backpressure instead
        // of buffering whatever the destination streams while the caller
        // waits, and a line longer than any MCP message ends the session
        // instead of growing a string until this process is killed.
        let (sender, lines) = sync_channel(PROBE_LINE_QUEUE);
        let line_cap = Arc::new(AtomicU64::new(MAX_PROBE_LINE_BYTES));
        let reader_cap = Arc::clone(&line_cap);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let event = match read_bounded_line(&mut reader, &reader_cap) {
                    Ok(BoundedLine::Line(line)) => Ok(line),
                    Ok(BoundedLine::TooLong { limit }) => Err(LineTooLong { limit }),
                    Ok(BoundedLine::Eof) | Err(_) => break,
                };
                let refused = event.is_err();
                if sender.send(event).is_err() || refused {
                    break;
                }
            }
        });
        Ok(Self {
            destination,
            child,
            writer: Some(writer),
            lines,
            line_cap,
            deadline: Instant::now() + timeout,
            next_id: 0,
            worker_invocation: None,
        })
    }

    /// Start a fresh budget for the next phase, discarding whatever the
    /// previous phases left of the old one.
    pub(super) fn restart_budget(&mut self, timeout: Duration) {
        self.deadline = Instant::now() + timeout;
    }

    #[cfg(test)]
    pub(in crate::federated) fn handshake(&mut self) -> Result<(), OrbitError> {
        self.handshake_with_worker(None)
    }

    pub(super) fn handshake_with_worker(
        &mut self,
        binding: Option<&orbit_types::tool::WorkerInvocation>,
    ) -> Result<(), OrbitError> {
        if let Some(binding) = binding {
            binding.validate().map_err(OrbitError::InvalidInput)?;
        }
        self.worker_invocation = binding.cloned();
        let response = self.request_probe(
            "initialize",
            json!({
                "_meta": {"orbit": {"worker_invocation": binding}},
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "orbit-federated-mux",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        )?;
        let negotiated = response["result"]["protocolVersion"].as_str();
        if negotiated != Some(PROTOCOL_VERSION) {
            return Err(unreachable(
                &self.destination,
                format!("destination negotiated MCP protocol {negotiated:?}"),
            ));
        }
        self.notify("notifications/initialized")
    }

    /// Call the destination's private federated discovery path and return its
    /// envelope. The public v1 list intentionally filters Invalid workspaces.
    pub(super) fn discover_workspaces(
        &mut self,
        arguments: Value,
    ) -> Result<DestinationSnapshot, OrbitError> {
        let response = self.request_probe(
            "tools/call",
            json!({
                "name": crate::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL,
                "arguments": arguments,
            }),
        )?;
        let result = &response["result"];
        let content = &result["structuredContent"];
        if result["isError"].as_bool().unwrap_or(false) {
            // The destination's named code survives: wrapping it in a fresh
            // message here would leave the caller matching on prose.
            return Err(remote_tool_error(&self.destination, content));
        }
        snapshot_from_discovery_content(&self.destination, content)
    }

    pub(super) fn list_tool_definitions(&mut self) -> Result<Vec<Value>, OrbitError> {
        let response = self.request_probe("tools/list", json!({}))?;
        response["result"]["tools"]
            .as_array()
            .cloned()
            .ok_or_else(|| {
                unreachable(
                    &self.destination,
                    "tools/list answer carried no tools array".to_string(),
                )
            })
    }

    /// Deliver one routed tool call.
    ///
    /// Unlike every other request here this one can commit work on the
    /// destination, so a lost answer after the request is written is
    /// [`LostAnswer::OutcomeUnknown`] rather than an unreachable host.
    pub(super) fn call_tool(&mut self, name: &str, arguments: Value) -> Result<Value, OrbitError> {
        // Before the request is written, so the reader cannot be holding the
        // probe ceiling when the (possibly large) result starts arriving.
        self.line_cap
            .store(MAX_TOOL_RESULT_LINE_BYTES, Ordering::Release);
        let response = self.request(
            "tools/call",
            json!({
                "name": mcp_advertised_tool_name(name),
                "arguments": arguments,
            }),
            LostAnswer::OutcomeUnknown { tool: name },
        )?;
        let result = &response["result"];
        let content = &result["structuredContent"];
        if result["isError"].as_bool().unwrap_or(false) {
            // Named destination codes such as `capability_refused` must survive
            // as `RemoteTool`, not be wrapped into `execution_failed`.
            return Err(remote_tool_error(&self.destination, content));
        }
        if content.is_null() {
            return Ok(json!({}));
        }
        Ok(content.clone())
    }

    /// Deliver without a public tool schema. The original arguments, including
    /// the durable admission request ID, are forwarded unchanged.
    pub(in crate::federated) fn call_internal_drain(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<Value, OrbitError> {
        self.line_cap
            .store(MAX_TOOL_RESULT_LINE_BYTES, Ordering::Release);
        let response = self.request(
            crate::internal_drain::CALL_METHOD,
            json!({"protocol": crate::INTERNAL_DRAIN_PROTOCOL, "name": name, "arguments": arguments}),
            LostAnswer::OutcomeUnknown { tool: name },
        )?;
        let result = &response["result"];
        let content = &result["structuredContent"];
        if result["isError"].as_bool().unwrap_or(false) {
            return Err(remote_tool_error(&self.destination, content));
        }
        Ok(content.clone())
    }

    /// A request whose loss tells the caller nothing was delivered.
    pub(super) fn request_probe(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, OrbitError> {
        self.request(method, params, LostAnswer::Unreachable)
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        lost: LostAnswer<'_>,
    ) -> Result<Value, OrbitError> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(
            method,
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }),
            |destination, reason| lost.classify(destination, id, reason),
        )?;
        self.await_response(method, id, lost)
    }

    fn notify(&mut self, method: &str) -> Result<(), OrbitError> {
        self.send(
            method,
            &json!({ "jsonrpc": "2.0", "method": method }),
            unreachable,
        )
    }

    /// Write one message line within the current deadline.
    ///
    /// A failed write is pre-dispatch by construction: the destination never
    /// saw a whole request, so it stays an unreachable host even for a
    /// delivery. A write still blocked at the deadline — a destination that
    /// stopped reading, or a stalled transport — kills the session so the
    /// write can end, and `landed` names the loss only if the whole line may
    /// have reached the destination before that.
    fn send(
        &mut self,
        method: &str,
        message: &Value,
        landed: impl FnOnce(&Destination, String) -> OrbitError,
    ) -> Result<(), OrbitError> {
        let mut line = serde_json::to_vec(message).map_err(|error| {
            OrbitError::Execution(format!("serialize federated probe request: {error}"))
        })?;
        line.push(b'\n');
        if Instant::now() >= self.deadline {
            return Err(unreachable(
                &self.destination,
                format!("budget spent before '{method}' was written"),
            ));
        }
        let writer = self.writer.take().ok_or_else(|| {
            unreachable(
                &self.destination,
                "session closed after an earlier write failed".to_string(),
            )
        })?;
        if writer.outbox.send(line).is_err() {
            return Err(unreachable(
                &self.destination,
                "write failed: session input closed".to_string(),
            ));
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        match writer.acks.recv_timeout(remaining) {
            Ok(Ok(())) => {
                self.writer = Some(writer);
                Ok(())
            }
            Ok(Err(error)) => Err(unreachable(
                &self.destination,
                format!("write failed: {error}"),
            )),
            Err(RecvTimeoutError::Disconnected) => Err(unreachable(
                &self.destination,
                "write failed: session input closed".to_string(),
            )),
            Err(RecvTimeoutError::Timeout) => {
                // Killing the child closes its end of the pipe, which is the
                // only way to end a write the destination is not draining.
                if let Err(error) = self.child.kill() {
                    tracing::debug!(
                        machine_id = %self.destination.machine_id,
                        %error,
                        "federated probe session was already gone"
                    );
                }
                let reason = format!("timed out writing '{method}'");
                match writer.acks.recv_timeout(WRITE_SETTLE_GRACE) {
                    // The line never fully left, so nothing ran.
                    Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => {
                        Err(unreachable(&self.destination, reason))
                    }
                    // Finished at the deadline, or unconfirmed either way.
                    Ok(Ok(())) | Err(RecvTimeoutError::Timeout) => {
                        Err(landed(&self.destination, reason))
                    }
                }
            }
        }
    }

    /// Read until a response with this id arrives or the deadline passes.
    /// Peer requests have their own ID namespace and must be answered before
    /// correlating responses, including when their ID equals ours.
    fn await_response(
        &mut self,
        method: &str,
        id: i64,
        lost: LostAnswer<'_>,
    ) -> Result<Value, OrbitError> {
        loop {
            // Checked before every read: a zero-length wait still returns a
            // line that is already queued, so a destination streaming
            // unrelated messages would otherwise outlast any deadline.
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            let received = if remaining.is_zero() {
                Err(RecvTimeoutError::Timeout)
            } else {
                self.lines.recv_timeout(remaining)
            };
            let line = match received {
                Ok(Ok(line)) => line,
                Ok(Err(LineTooLong { limit })) => {
                    return Err(lost.classify(
                        &self.destination,
                        id,
                        format!(
                            "answer to '{method}' exceeded the {limit}-byte line limit and was refused"
                        ),
                    ));
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(lost.classify(
                        &self.destination,
                        id,
                        format!("timed out waiting for '{method}'"),
                    ));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(lost.classify(
                        &self.destination,
                        id,
                        format!("session ended before answering '{method}'"),
                    ));
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = match serde_json::from_str(line.trim()) {
                Ok(message) => message,
                Err(error) => {
                    return Err(lost.classify(
                        &self.destination,
                        id,
                        format!("emitted invalid JSON while awaiting '{method}': {error}"),
                    ));
                }
            };
            if let Some(peer_method) = message.get("method") {
                // Notifications do not require a reply. Preserve numeric and
                // string request IDs verbatim; neither belongs to our counter.
                if let Some(peer_id) = message
                    .get("id")
                    .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
                {
                    self.answer_peer(peer_id, peer_method).map_err(|error| {
                        // The outgoing call is already dispatched. A failed
                        // peer reply cannot reclassify it as a delivery miss.
                        lost.classify(
                            &self.destination,
                            id,
                            format!("could not answer peer while awaiting '{method}': {error}"),
                        )
                    })?;
                }
                continue;
            }
            if message.get("id").and_then(Value::as_i64) == Some(id) {
                if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
                    || message.get("result").is_some() == message.get("error").is_some()
                {
                    return Err(lost.classify(
                        &self.destination,
                        id,
                        format!("emitted an invalid JSON-RPC response while awaiting '{method}'"),
                    ));
                }
                if let Some(error) = message.get("error") {
                    return Err(unreachable(
                        &self.destination,
                        format!("'{method}' failed: {error}"),
                    ));
                }
                return Ok(message);
            }
        }
    }

    /// This client advertises no optional capabilities. Ping is required;
    /// other peer methods are explicitly refused. Sending uses the current
    /// absolute deadline, so servicing requests never renews the call budget.
    fn answer_peer(&mut self, id: &Value, method: &Value) -> Result<(), OrbitError> {
        let response = match method.as_str() {
            Some("ping") => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            Some(_) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "Method not found"},
            }),
            None => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32600, "message": "Invalid request"},
            }),
        };
        self.send("peer response", &response, unreachable)
    }
}

/// The reader refused a line that outgrew the cap of the phase in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LineTooLong {
    limit: u64,
}

/// What one bounded read produced.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BoundedLine {
    /// One line, including its trailing newline when the stream had one.
    Line(String),
    /// The line exceeded the cap before its newline arrived.
    TooLong { limit: u64 },
    /// The stream ended with nothing pending.
    Eof,
}

/// Read one line without ever holding more than the current cap.
///
/// The cap is re-read on every buffer refill rather than fixed per line, so a
/// phase change that raises it takes effect for a line the reader is already
/// blocked on. Invalid UTF-8 is an error, as it was for `read_line`.
pub(super) fn read_bounded_line(
    reader: &mut impl BufRead,
    cap: &AtomicU64,
) -> std::io::Result<BoundedLine> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let limit = cap.load(Ordering::Acquire);
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            if line.is_empty() {
                return Ok(BoundedLine::Eof);
            }
            break;
        }
        let (take, complete) = match chunk.iter().position(|byte| *byte == b'\n') {
            Some(newline) => (newline + 1, true),
            None => (chunk.len(), false),
        };
        if (line.len() + take) as u64 > limit {
            return Ok(BoundedLine::TooLong { limit });
        }
        line.extend_from_slice(&chunk[..take]);
        reader.consume(take);
        if complete {
            break;
        }
    }
    String::from_utf8(line)
        .map(BoundedLine::Line)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// The session's stdin, owned by a thread so a write the destination never
/// drains cannot hold the caller past its deadline.
///
/// Each line is acknowledged once fully written and flushed; the thread ends
/// after the first failed write or when the session drops its sender, and a
/// write blocked on a killed child fails as soon as the pipe closes.
struct RequestWriter {
    outbox: SyncSender<Vec<u8>>,
    acks: Receiver<std::io::Result<()>>,
}

impl RequestWriter {
    fn spawn(mut stdin: std::process::ChildStdin) -> Self {
        let (outbox, pending) = sync_channel::<Vec<u8>>(1);
        let (acknowledge, acks) = sync_channel(1);
        std::thread::spawn(move || {
            for line in pending {
                let written = stdin.write_all(&line).and_then(|()| stdin.flush());
                let failed = written.is_err();
                if acknowledge.send(written).is_err() || failed {
                    break;
                }
            }
        });
        Self { outbox, acks }
    }
}

impl Drop for DestinationSession {
    fn drop(&mut self) {
        if let Err(error) = self.child.kill() {
            tracing::debug!(
                machine_id = %self.destination.machine_id,
                %error,
                "federated probe session was already gone"
            );
        }
        // Reap it: an unwaited SSH child would linger as a zombie for the life
        // of this long-running server process, once per destination per call.
        let _ = self.child.wait();
    }
}
