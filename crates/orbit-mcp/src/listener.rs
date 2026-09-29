//! TCP listener transport for the MCP kernel.
//!
//! The protocol handler performs no IO of its own, so serving it over a socket
//! is only a question of where the byte stream comes from, who owns the session
//! behind it, and which addresses may be bound. Framing, dispatch, and the
//! trusted session envelope are shared with stdio. Before handing a socket to
//! rmcp, this module rejects streams that do not start with a JSON object.
//! Without that guard, rmcp skips HTTP request lines and headers, then dispatches
//! JSON-RPC lines in a browser's HTTP POST body. Loopback alone does not stop a
//! page visited on the same machine from reaching the socket.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use rmcp::ServiceExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{McpHost, OrbitToolServer};

/// Loopback port `orbit mcp listen` binds when no address is given.
pub const DEFAULT_MCP_LISTEN_PORT: u16 = 7879;

/// Sessions one listener serves at a time. Each accepted connection holds a
/// server instance and an rmcp session until the peer closes, and the
/// listener authenticates no one, so without a ceiling any process that can
/// reach the socket can pin descriptors and memory until `accept` itself
/// fails. Beyond the ceiling new connections wait for a slot rather than
/// being refused, so a burst of legitimate clients degrades to queueing.
pub const DEFAULT_MAX_MCP_SESSIONS: usize = 64;

/// How long the accept loop pauses after a resource-exhaustion error before
/// trying again, instead of spinning or giving up.
const ACCEPT_EXHAUSTION_BACKOFF: Duration = Duration::from_millis(250);

/// How long an accepted connection may stay silent before the listener drops
/// it. The peer must send the first byte of a JSON object promptly: a real
/// client writes `initialize` immediately, while a peer that connects and sends
/// nothing would otherwise hold one of the session permits indefinitely, and
/// enough of them starve every legitimate client.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(5);

/// How far a listener is allowed to be reachable.
///
/// The listener authenticates no one: a local process that reaches the socket
/// gets the accepting machine's agent tool surface. Loopback blocks remote TCP
/// peers, but a browser page can send HTTP requests to loopback. The framing
/// guard below closes those requests before rmcp can dispatch their bodies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ListenerExposure {
    /// Refuse any bind address that is not a loopback address.
    #[default]
    LoopbackOnly,
    /// Bind the requested address even when other hosts can reach it. The
    /// deployment has taken on restricting access by other means.
    AnyInterface,
}

/// A bound MCP endpoint that serves each accepted connection as its own
/// session.
pub struct McpListener {
    listener: TcpListener,
    host: Arc<dyn McpHost>,
    trusted_context: ToolSessionContext,
    sessions: Arc<Semaphore>,
    first_byte_timeout: Duration,
}

impl McpListener {
    /// Apply the bind policy and bind the endpoint without accepting anything
    /// yet.
    ///
    /// Binding is separated from accepting so a caller can report the assigned
    /// address — notably when binding port 0 — before connections arrive.
    pub async fn bind(
        addr: SocketAddr,
        exposure: ListenerExposure,
        host: Arc<dyn McpHost>,
        trusted_context: ToolSessionContext,
    ) -> Result<Self, OrbitError> {
        ensure_bind_allowed(addr, exposure)?;
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|error| OrbitError::Execution(format!("mcp listen bind {addr}: {error}")))?;
        Ok(Self {
            listener,
            host,
            trusted_context,
            sessions: Arc::new(Semaphore::new(DEFAULT_MAX_MCP_SESSIONS)),
            first_byte_timeout: FIRST_BYTE_TIMEOUT,
        })
    }

    /// Replace how long an accepted connection may stay silent before it is
    /// closed and its session slot released.
    pub fn with_first_byte_timeout(mut self, timeout: Duration) -> Self {
        self.first_byte_timeout = timeout;
        self
    }

    /// The address actually bound, including any kernel-assigned port.
    pub fn local_addr(&self) -> Result<SocketAddr, OrbitError> {
        self.listener
            .local_addr()
            .map_err(|error| OrbitError::Execution(format!("mcp listen local address: {error}")))
    }

    /// Accept connections until the listener itself fails.
    ///
    /// Each connection is served on its own task with its own server instance.
    /// That isolation is load-bearing rather than defensive: the adapter's
    /// session state is written during `initialize`, so two clients sharing one
    /// instance would race on the announced workspace selector and the loser
    /// would receive a successful response computed against the other client's
    /// workspace.
    pub async fn serve(self) -> Result<(), OrbitError> {
        loop {
            // Take the slot before accepting so a full listener leaves the
            // connection in the kernel backlog instead of holding an accepted
            // socket it cannot serve yet.
            let permit = Arc::clone(&self.sessions)
                .acquire_owned()
                .await
                .map_err(|_| OrbitError::Execution("mcp listen session gate closed".to_string()))?;
            let (stream, peer) = match self.listener.accept().await {
                Ok(accepted) => accepted,
                // A connection that died between the SYN and our accept is the
                // remote's problem. Anything else means the listener is no
                // longer usable, and spinning on it would burn the accept loop.
                Err(error) if is_transient_accept_error(&error) => {
                    tracing::warn!(error = %error, "mcp listener skipped a connection");
                    continue;
                }
                // Out of descriptors (or memory): the sessions already being
                // served will release some. Ending the listener here would
                // take those healthy sessions' endpoint down with the burst.
                Err(error) if is_resource_exhaustion(&error) => {
                    tracing::warn!(error = %error, "mcp listener backing off after resource exhaustion");
                    drop(permit);
                    tokio::time::sleep(ACCEPT_EXHAUSTION_BACKOFF).await;
                    continue;
                }
                Err(error) => {
                    return Err(OrbitError::Execution(format!("mcp listen accept: {error}")));
                }
            };
            let server = OrbitToolServer::new_with_context(
                Arc::clone(&self.host),
                self.session_context_for(peer),
            );
            tokio::spawn(serve_connection(
                server,
                stream,
                peer,
                permit,
                self.first_byte_timeout,
            ));
        }
    }

    /// One session's trusted context: the listener's template, plus the peer
    /// address this process observed, minus the correlation identifiers so the
    /// adapter mints a fresh origin session id per connection. A listener-wide
    /// id would collapse concurrent clients into one audit identity.
    ///
    /// The peer address is a network observation, never an authenticated
    /// identity; it lands in the same audit field an SSH session fills from
    /// `SSH_CONNECTION`.
    fn session_context_for(&self, peer: SocketAddr) -> ToolSessionContext {
        let mut context = self.trusted_context.clone();
        context.origin_session_id = None;
        context.mcp_call_id = None;
        context.caller_ip = Some(peer.ip().to_string());
        context
    }
}

fn ensure_bind_allowed(addr: SocketAddr, exposure: ListenerExposure) -> Result<(), OrbitError> {
    if exposure == ListenerExposure::AnyInterface || addr.ip().is_loopback() {
        return Ok(());
    }
    Err(OrbitError::InvalidInput(format!(
        "refusing to bind the MCP listener to non-loopback address {addr}: the listener \
         authenticates no client. Reach it through an authenticated tunnel, or pass \
         `--allow-non-loopback` once the network path is restricted by other means."
    )))
}

async fn serve_connection(
    server: OrbitToolServer,
    stream: TcpStream,
    peer: SocketAddr,
    _permit: OwnedSemaphorePermit,
    first_byte_timeout: Duration,
) {
    // rmcp ignores unparsable lines. An HTTP request line and its headers are
    // unparsable, but JSON-RPC lines in a POST body are not. Check the first
    // byte without consuming it so only a JSON object can start a session.
    // The wait is bounded because this task holds a session permit: a silent
    // peer must not keep it.
    let mut first = [0];
    match tokio::time::timeout(first_byte_timeout, stream.peek(&mut first)).await {
        Ok(Ok(1)) if first[0] == b'{' => {}
        Ok(Ok(_)) => {
            tracing::debug!(peer = %peer, "mcp listener rejected non-JSON framing");
            return;
        }
        Ok(Err(error)) => {
            tracing::debug!(peer = %peer, error = %error, "mcp listener could not read first byte");
            return;
        }
        Err(_) => {
            tracing::warn!(
                peer = %peer,
                timeout_ms = first_byte_timeout.as_millis() as u64,
                "mcp listener closed a connection that sent nothing"
            );
            return;
        }
    }
    let running = match server.serve(stream).await {
        Ok(running) => running,
        Err(error) => {
            tracing::warn!(peer = %peer, error = %error, "mcp listener session did not start");
            return;
        }
    };
    if let Err(error) = running.waiting().await {
        tracing::warn!(peer = %peer, error = %error, "mcp listener session ended with an error");
    }
}

fn is_transient_accept_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset | ErrorKind::Interrupted
    )
}

/// `accept` failed for want of a descriptor or memory rather than because the
/// socket is gone: EMFILE / ENFILE (no stable `ErrorKind` on every platform,
/// so the raw errno is matched on Unix) or an out-of-memory report.
fn is_resource_exhaustion(error: &std::io::Error) -> bool {
    if error.kind() == ErrorKind::OutOfMemory {
        return true;
    }
    #[cfg(unix)]
    {
        // ENFILE (23) and EMFILE (24) share these values on Linux and macOS;
        // matching the errno keeps this crate free of a libc edge.
        matches!(error.raw_os_error(), Some(23 | 24))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
#[path = "tests/listener.rs"]
mod tests;
