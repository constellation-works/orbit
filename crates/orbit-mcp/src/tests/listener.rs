//! Bind-policy and idle-peer tests for the MCP TCP listener.

use super::*;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

struct NoTools;

impl McpHost for NoTools {
    fn list_mcp_tool_definitions(
        &self,
    ) -> Result<Vec<orbit_types::tool::McpToolDefinition>, OrbitError> {
        Ok(Vec::new())
    }

    fn call_tool(
        &self,
        name: &str,
        _input: Value,
        _session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::InvalidInput(format!("no tool {name}")))
    }
}

fn addr(text: &str) -> SocketAddr {
    text.parse().expect("socket address")
}

#[test]
fn non_loopback_addresses_are_refused_before_the_socket_opens() {
    let error = ensure_bind_allowed(addr("0.0.0.0:7879"), ListenerExposure::LoopbackOnly)
        .expect_err("wildcard bind must be refused");
    let OrbitError::InvalidInput(message) = error else {
        panic!("expected an invalid-input refusal");
    };
    assert!(message.contains("0.0.0.0:7879"), "{message}");
    assert!(message.contains("--allow-non-loopback"), "{message}");
}

#[tokio::test]
async fn a_peer_that_sends_nothing_is_disconnected_and_releases_its_slot() {
    let listener = McpListener::bind(
        addr("127.0.0.1:0"),
        ListenerExposure::LoopbackOnly,
        Arc::new(NoTools),
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind listener")
    .with_first_byte_timeout(Duration::from_millis(100));
    let bound = listener.local_addr().expect("bound address");
    let sessions = Arc::clone(&listener.sessions);
    let server = tokio::spawn(listener.serve());

    let mut idle = TcpStream::connect(bound).await.expect("connect");
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(10), idle.read(&mut buf))
        .await
        .expect("the listener must close a silent connection instead of holding it");
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "a silent peer gets no data, only a close: {read:?}"
    );

    // The slot came back, so silent peers cannot exhaust the session ceiling.
    // The accept loop keeps one permit in hand while it waits for the next
    // connection, so a fully idle listener shows one fewer than the ceiling.
    let released = tokio::time::timeout(Duration::from_secs(10), async {
        while sessions.available_permits() != DEFAULT_MAX_MCP_SESSIONS - 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        released.is_ok(),
        "the idle peer's session permit must be released"
    );
    server.abort();
}

#[tokio::test]
async fn a_peer_that_streams_an_endless_message_is_disconnected() {
    let listener = McpListener::bind(
        addr("127.0.0.1:0"),
        ListenerExposure::LoopbackOnly,
        Arc::new(NoTools),
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind listener")
    .with_max_message_bytes(4 * 1024);
    let bound = listener.local_addr().expect("bound address");
    let server = tokio::spawn(listener.serve());

    let mut client = TcpStream::connect(bound).await.expect("connect");
    client
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}
"#,
        )
        .await
        .expect("send initialize");
    let mut reply = [0u8; 1];
    client
        .read_exact(&mut reply)
        .await
        .expect("initialize is answered");

    // A message with no newline, longer than the ceiling. The server must
    // hang up rather than buffer it for as long as the peer keeps sending.
    let mut chunk = vec![b'a'; 1024];
    chunk[0] = b'{';
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        for _ in 0..64 {
            if client.write_all(&chunk).await.is_err() {
                return;
            }
        }
        let mut sink = Vec::new();
        let _ = client.read_to_end(&mut sink).await;
    })
    .await;
    assert!(
        closed.is_ok(),
        "the listener must close a session whose message exceeds the size ceiling"
    );
    server.abort();
}
