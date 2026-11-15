//! Initialization deadlines and session recovery through the actual TCP API.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_mcp::{ListenerExposure, McpHost, McpListener};
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use rmcp::ServiceExt;
use rmcp::model::ClientConfig;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

struct NoTools;

/// Records whether a tool call was dispatched. The refusal test admits a uid
/// other than this process, so the flag must stay clear.
struct RecordingHost {
    called: Arc<AtomicBool>,
}

impl McpHost for NoTools {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        Ok(Vec::new())
    }

    fn call_tool(
        &self,
        name: &str,
        _input: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::InvalidInput(format!("no tool {name}")))
    }
}

impl McpHost for RecordingHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        Ok(Vec::new())
    }

    fn call_tool(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.called.store(true, Ordering::SeqCst);
        Ok(Value::Null)
    }
}

async fn listen(budget: Duration) -> (SocketAddr, tokio::task::JoinHandle<Result<(), OrbitError>>) {
    let listener = McpListener::bind(
        "127.0.0.1:0".parse().expect("loopback address"),
        ListenerExposure::LoopbackOnly,
        Arc::new(NoTools),
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind listener")
    .with_initialization_timeout(budget);
    let addr = listener.local_addr().expect("bound address");
    (addr, tokio::spawn(listener.serve()))
}

async fn assert_closed(stream: &mut (impl tokio::io::AsyncRead + Unpin), wait: Duration) {
    let mut byte = [0];
    let read = tokio::time::timeout(wait, stream.read(&mut byte))
        .await
        .expect("initialization deadline must close the stalled connection");
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "stalled initialization must receive only a close: {read:?}"
    );
}

#[tokio::test]
async fn partial_json_is_closed_within_the_initialization_budget() {
    let budget = Duration::from_millis(200);
    let (addr, accepting) = listen(budget).await;
    let mut stalled = TcpStream::connect(addr).await.expect("connect");
    stalled.write_all(b"{").await.expect("send opening byte");
    assert_closed(&mut stalled, budget * 2).await;
    accepting.abort();
}

#[tokio::test]
async fn the_first_byte_does_not_restart_the_initialization_budget() {
    let budget = Duration::from_secs(1);
    let (addr, accepting) = listen(budget).await;
    let mut stalled = TcpStream::connect(addr).await.expect("connect");
    tokio::time::sleep(Duration::from_millis(700)).await;
    stalled.write_all(b"{").await.expect("send delayed byte");
    assert_closed(&mut stalled, Duration::from_millis(500)).await;
    accepting.abort();
}

#[tokio::test]
async fn a_full_listener_recovers_without_partial_peers_closing_themselves() {
    let budget = Duration::from_secs(2);
    let (addr, accepting) = listen(budget).await;
    let mut stalled = Vec::new();
    // A pre-initialize ping response proves each socket has been accepted and
    // holds a slot. Its following opening brace leaves initialize incomplete.
    // Fill the listener's 64-session ceiling without closing any client socket.
    for _ in 0..64 {
        let mut stream = BufReader::new(TcpStream::connect(addr).await.expect("connect"));
        stream
            .get_mut()
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"ping\"}\n{")
            .await
            .expect("send pre-initialize ping and partial JSON");
        let mut reply = String::new();
        tokio::time::timeout(budget, stream.read_line(&mut reply))
            .await
            .expect("every stalled peer must occupy an accepted session")
            .expect("read ping response");
        let response: Value = serde_json::from_str(&reply).expect("JSON-RPC ping response");
        assert_eq!(response["id"], 0);
        assert!(response.get("result").is_some(), "ping must succeed");
        stalled.push(stream);
    }

    let stream = TcpStream::connect(addr)
        .await
        .expect("connect legitimate peer");
    let initialize = ClientConfig::default().serve(stream);
    tokio::pin!(initialize);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut initialize)
            .await
            .is_err(),
        "all session slots must be occupied before the deadline expires"
    );
    let client = tokio::time::timeout(budget * 2, &mut initialize)
        .await
        .expect("expired initialization must make a slot available")
        .expect("legitimate peer initializes while all stalled sockets remain open");
    client
        .peer()
        .list_tools(None)
        .await
        .expect("recovered session serves requests");
    for stream in &mut stalled {
        assert_closed(stream, budget).await;
    }
    client.cancel().await.expect("close legitimate session");
    accepting.abort();
}

#[tokio::test]
async fn initialized_sessions_remain_usable_after_the_initialization_budget() {
    let budget = Duration::from_millis(200);
    let (addr, accepting) = listen(budget).await;
    let stream = TcpStream::connect(addr).await.expect("connect");
    let client = tokio::time::timeout(budget, ClientConfig::default().serve(stream))
        .await
        .expect("initialize promptly")
        .expect("initialize succeeds");
    tokio::time::sleep(budget * 2).await;
    let listed = tokio::time::timeout(budget, client.peer().list_tools(None))
        .await
        .expect("idle established session remains responsive")
        .expect("idle established session is still connected");
    assert!(listed.tools.is_empty());
    client.cancel().await.expect("close session");
    accepting.abort();
}

/// Sessions one listener serves at once. Matches the listener's ceiling; the
/// permit gate itself is not part of the public API, so release is observed
/// by whether another client can start.
const SESSION_CEILING: usize = 64;

#[tokio::test]
async fn non_loopback_addresses_are_refused_before_the_socket_opens() {
    let bound = McpListener::bind(
        "0.0.0.0:7879".parse().expect("socket address"),
        ListenerExposure::LoopbackOnly,
        Arc::new(NoTools),
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await;
    let Err(error) = bound else {
        panic!("wildcard bind must be refused");
    };
    let OrbitError::InvalidInput(message) = error else {
        panic!("expected an invalid-input refusal");
    };
    assert!(message.contains("0.0.0.0:7879"), "{message}");
    assert!(message.contains("--allow-non-loopback"), "{message}");
}

#[tokio::test]
async fn a_peer_that_sends_nothing_is_disconnected_and_releases_its_slot() {
    let (addr, accepting) = listen(Duration::from_millis(100)).await;
    let mut idle = TcpStream::connect(addr).await.expect("connect");
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(10), idle.read(&mut buf))
        .await
        .expect("the listener must close a silent connection instead of holding it");
    assert!(
        matches!(read, Ok(0) | Err(_)),
        "a silent peer gets no data, only a close: {read:?}"
    );
    accepting.abort();

    let budget = Duration::from_secs(2);
    let (addr, accepting) = listen(budget).await;
    let mut silent = Vec::new();
    for _ in 0..SESSION_CEILING {
        silent.push(TcpStream::connect(addr).await.expect("connect silent peer"));
    }
    let stream = TcpStream::connect(addr)
        .await
        .expect("connect legitimate peer");
    let initialize = ClientConfig::default().serve(stream);
    tokio::pin!(initialize);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut initialize)
            .await
            .is_err(),
        "silent peers must occupy every session slot"
    );
    let client = tokio::time::timeout(budget * 2, &mut initialize)
        .await
        .expect("closing silent peers must release a session slot")
        .expect("legitimate peer initializes");
    for stream in &mut silent {
        assert_closed(stream, budget).await;
    }
    client.cancel().await.expect("close legitimate session");
    accepting.abort();
}

#[tokio::test]
async fn a_peer_that_streams_an_endless_message_is_disconnected() {
    let listener = McpListener::bind(
        "127.0.0.1:0".parse().expect("loopback address"),
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

#[tokio::test]
async fn a_different_uid_is_refused_before_any_tool_call() {
    let called = Arc::new(AtomicBool::new(false));
    let listener = McpListener::bind(
        "127.0.0.1:0".parse().expect("loopback address"),
        ListenerExposure::LoopbackOnly,
        Arc::new(RecordingHost {
            called: Arc::clone(&called),
        }),
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind listener")
    .with_allowed_peer_uid(McpListener::current_euid().wrapping_add(1));
    let addr = listener.local_addr().expect("bound address");
    let server = tokio::spawn(listener.serve());

    let mut client = TcpStream::connect(addr).await.expect("connect");
    // initialize is answered on an admitted session (see the same-user tests).
    // A different uid must close the socket before that response, and before
    // call_tool can run.
    let _ = client
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"probe","arguments":{}}}
"#,
        )
        .await;
    let mut buf = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut buf)).await;
    match read {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => {}
        Err(_) => panic!("a different-uid peer must be closed before any MCP response"),
    }
    assert!(
        buf.is_empty(),
        "a different-uid peer is refused before any MCP response: {buf:?}"
    );
    assert!(
        !called.load(Ordering::SeqCst),
        "a different-uid peer must be refused before any tool call"
    );
    server.abort();
}

/// The kernel table has two established rows. Only the client row may admit.
/// A lookup that reads the reversed server row would accept this peer, because
/// that row carries this process's uid.
#[tokio::test]
async fn the_client_row_is_refused_when_the_server_row_matches() {
    let called = Arc::new(AtomicBool::new(false));
    let euid = McpListener::current_euid();
    let listener = McpListener::bind(
        "127.0.0.1:0".parse().expect("loopback address"),
        ListenerExposure::LoopbackOnly,
        Arc::new(RecordingHost {
            called: Arc::clone(&called),
        }),
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind listener")
    .with_tcp_uid_rows(euid.wrapping_add(1), euid);
    let addr = listener.local_addr().expect("bound address");
    let server = tokio::spawn(listener.serve());

    let mut client = TcpStream::connect(addr).await.expect("connect");
    let _ = client
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"probe","arguments":{}}}
"#,
        )
        .await;
    let mut buf = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(2), client.read_to_end(&mut buf)).await;
    match read {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => {}
        Err(_) => {
            panic!("a client row with a different uid must be closed before any MCP response")
        }
    }
    assert!(
        buf.is_empty(),
        "the reversed server row must not admit the peer: {buf:?}"
    );
    assert!(
        !called.load(Ordering::SeqCst),
        "a client row with a different uid must be refused before any tool call"
    );
    server.abort();
}

/// The reversed row carries a different uid. Admission has to follow the
/// client row, or this same-user peer would be refused.
#[tokio::test]
async fn the_client_row_is_admitted_when_the_server_row_differs() {
    let euid = McpListener::current_euid();
    let listener = McpListener::bind(
        "127.0.0.1:0".parse().expect("loopback address"),
        ListenerExposure::LoopbackOnly,
        Arc::new(NoTools),
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind listener")
    .with_tcp_uid_rows(euid, euid.wrapping_add(1));
    let addr = listener.local_addr().expect("bound address");
    let server = tokio::spawn(listener.serve());

    let mut client = BufReader::new(TcpStream::connect(addr).await.expect("connect"));
    client
        .get_mut()
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"ping\"}\n")
        .await
        .expect("send ping");
    let mut reply = String::new();
    tokio::time::timeout(Duration::from_secs(2), client.read_line(&mut reply))
        .await
        .expect("a client row owned by this user must be admitted")
        .expect("read ping");
    let response: Value = serde_json::from_str(&reply).expect("JSON-RPC ping response");
    assert_eq!(response["id"], 0);
    assert!(
        response.get("result").is_some(),
        "ping must succeed when only the client row matches: {reply}"
    );
    server.abort();
}
