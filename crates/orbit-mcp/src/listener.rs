//! TCP listener transport for the MCP kernel.
//!
//! The protocol handler performs no IO of its own, so serving it over a socket
//! is only a question of where the byte stream comes from, who owns the session
//! behind it, and which addresses may be bound. Framing, dispatch, and the
//! trusted session envelope are shared with stdio. Before handing a socket to
//! rmcp, this module closes a peer whose user id is not the server's, then
//! rejects streams that do not start with a JSON object. Without that framing
//! guard, rmcp skips HTTP request lines and headers, then dispatches JSON-RPC
//! lines in a browser's HTTP POST body. Loopback alone does not stop a page
//! visited on the same machine from reaching the socket.

use std::io::ErrorKind;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_types::tool::ToolSessionContext;
use rmcp::ServiceExt;
use tokio::io::{AsyncRead, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{McpHost, OrbitToolServer};

#[path = "listener_peer.rs"]
mod peer;

/// Loopback port `orbit mcp listen` binds when no address is given.
pub const DEFAULT_MCP_LISTEN_PORT: u16 = 7879;

/// Sessions one listener serves at a time. Each accepted connection holds a
/// server instance and an rmcp session until the peer closes. Only this
/// process's user is admitted, and without a ceiling that user can still pin
/// descriptors and memory until `accept` itself fails. Beyond the ceiling new
/// connections wait for a slot rather than
/// being refused, so a burst of legitimate clients degrades to queueing.
pub const DEFAULT_MAX_MCP_SESSIONS: usize = 64;

/// How long the accept loop pauses after a resource-exhaustion error before
/// trying again, instead of spinning or giving up.
const ACCEPT_EXHAUSTION_BACKOFF: Duration = Duration::from_millis(250);

/// How long an accepted connection has to complete initialization, including
/// the framing check, receiving the request, and sending its response. Silent
/// peers and incomplete messages must not hold session permits indefinitely.
const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest single JSON-RPC message, in bytes, a listener session accepts.
///
/// A same-user peer can still send one huge line. rmcp's transport discards a line longer than its
/// own 16 MiB default and keeps the connection open, so this stricter limit is
/// what protects the listener. A session whose single message exceeds it is
/// closed before rmcp buffers the message whole. Far larger than any tool call
/// an agent sends.
pub(crate) const DEFAULT_MAX_MCP_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// How far a listener is allowed to be reachable.
///
/// The listener admits a connection only when the peer's user id is the
/// server's, and then only to the agent tool surface. Loopback blocks remote
/// TCP peers; a remote peer also has no local user id and is refused. A
/// browser page can still send HTTP requests to loopback. The framing guard
/// below closes those requests before rmcp can dispatch their bodies.
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
    initialization_timeout: Duration,
    max_message_bytes: usize,
    /// Effective uid captured at bind. Production callers cannot replace it.
    allowed_peer_uid: u32,
    /// Test-only stand-in for the two established TCP rows. Production binds
    /// leave this empty, so the platform lookup is what admits a peer.
    #[cfg(feature = "test-support")]
    tcp_uid_rows: Option<peer::TcpUidRows>,
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
            initialization_timeout: INITIALIZATION_TIMEOUT,
            max_message_bytes: DEFAULT_MAX_MCP_MESSAGE_BYTES,
            allowed_peer_uid: peer::current_euid(),
            #[cfg(feature = "test-support")]
            tcp_uid_rows: None,
        })
    }

    /// Replace how long an accepted connection has to complete initialization
    /// before it is closed and its session slot released. Established sessions
    /// have no idle timeout.
    pub fn with_initialization_timeout(mut self, timeout: Duration) -> Self {
        self.initialization_timeout = timeout;
        self
    }

    /// Replace the longest message a session may send before it is closed.
    pub fn with_max_message_bytes(mut self, limit: usize) -> Self {
        self.max_message_bytes = limit;
        self
    }

    /// Admit only connections whose peer uid is `uid`, instead of this
    /// process. Tests use a uid other than the connector's to observe the
    /// refusal; production binds capture the effective uid themselves.
    #[cfg(feature = "test-support")]
    pub fn with_allowed_peer_uid(mut self, uid: u32) -> Self {
        self.allowed_peer_uid = uid;
        self
    }

    /// Effective uid of this process, the uid a production listener admits.
    #[cfg(feature = "test-support")]
    pub fn current_euid() -> u32 {
        peer::current_euid()
    }

    /// Stand in the two established TCP rows for this listener.
    ///
    /// `client_uid` is the socket whose local address is the accepted peer.
    /// `server_uid` is the reversed row. Production callers have no setter.
    #[cfg(feature = "test-support")]
    pub fn with_tcp_uid_rows(mut self, client_uid: u32, server_uid: u32) -> Self {
        self.tcp_uid_rows = Some(peer::TcpUidRows::new(client_uid, server_uid));
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
            // The user-id check runs before any read, so a refused peer never
            // reaches framing or tool dispatch. A local-address failure fails
            // closed for the same reason.
            let local = match stream.local_addr() {
                Ok(addr) => addr,
                Err(error) => {
                    tracing::warn!(
                        peer = %peer,
                        error = %error,
                        "mcp listener refused a peer whose local address could not be read"
                    );
                    drop(permit);
                    continue;
                }
            };
            #[cfg(feature = "test-support")]
            let tcp_uid_rows = self.tcp_uid_rows;
            #[cfg(not(feature = "test-support"))]
            let tcp_uid_rows = None;
            if let Some(reason) =
                peer::peer_refusal(local, peer, self.allowed_peer_uid, tcp_uid_rows)
            {
                tracing::warn!(
                    peer = %peer,
                    allowed_uid = self.allowed_peer_uid,
                    reason,
                    "mcp listener refused a peer before reading"
                );
                drop(permit);
                continue;
            }
            let initialization_deadline = tokio::time::Instant::now() + self.initialization_timeout;
            let server = OrbitToolServer::new_with_context(
                Arc::clone(&self.host),
                self.session_context_for(peer),
            );
            tokio::spawn(serve_connection(
                server,
                stream,
                peer,
                permit,
                initialization_deadline,
                self.max_message_bytes,
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
        "refusing to bind the MCP listener to non-loopback address {addr}: pass \
         `--allow-non-loopback` only when every peer is this user. A remote TCP \
         client has no local user id and is refused. Reach the listener through an \
         SSH tunnel as this user, or keep the bind on loopback."
    )))
}

async fn serve_connection(
    server: OrbitToolServer,
    stream: TcpStream,
    peer: SocketAddr,
    _permit: OwnedSemaphorePermit,
    initialization_deadline: tokio::time::Instant,
    max_message_bytes: usize,
) {
    // rmcp ignores unparsable lines. An HTTP request line and its headers are
    // unparsable, but JSON-RPC lines in a POST body are not. Check the first
    // byte without consuming it so only a JSON object can start a session.
    // Both this peek and rmcp initialization share the deadline set at accept:
    // receiving one byte must not buy a peer another budget to hold the slot.
    let mut first = [0];
    match tokio::time::timeout_at(initialization_deadline, stream.peek(&mut first)).await {
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
                "mcp listener closed a connection that sent nothing"
            );
            return;
        }
    }
    let (reader, writer) = tokio::io::split(stream);
    let transport = (MessageLimited::new(reader, max_message_bytes), writer);
    let running =
        match tokio::time::timeout_at(initialization_deadline, server.serve(transport)).await {
            Ok(Ok(running)) => running,
            Ok(Err(error)) => {
                tracing::warn!(peer = %peer, error = %error, "mcp listener session did not start");
                return;
            }
            Err(_) => {
                // Dropping the initialization future closes its transport; returning
                // also releases the session permit. Only established sessions wait
                // below, outside the initialization deadline.
                tracing::warn!(peer = %peer, "mcp listener initialization timed out");
                return;
            }
        };
    if let Err(error) = running.waiting().await {
        tracing::warn!(peer = %peer, error = %error, "mcp listener session ended with an error");
    }
}

/// Fails a read once a message (the bytes since the last newline) outgrows
/// `limit`, which ends the rmcp session before the message is buffered whole.
struct MessageLimited<R> {
    inner: R,
    limit: usize,
    line_bytes: usize,
}

impl<R> MessageLimited<R> {
    fn new(inner: R, limit: usize) -> Self {
        Self {
            inner,
            limit,
            line_bytes: 0,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for MessageLimited<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {}
            other => return other,
        }
        for byte in &buf.filled()[before..] {
            if *byte == b'\n' {
                this.line_bytes = 0;
            } else {
                this.line_bytes += 1;
                if this.line_bytes > this.limit {
                    return Poll::Ready(Err(std::io::Error::new(
                        ErrorKind::InvalidData,
                        format!("mcp message exceeds {} bytes", this.limit),
                    )));
                }
            }
        }
        Poll::Ready(Ok(()))
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
