//! Initialization deadlines and session recovery through the actual TCP API.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use orbit_common::OrbitError;
use orbit_mcp::{ListenerExposure, McpHost, McpListener};
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use rmcp::ServiceExt;
use rmcp::model::ClientInfo;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

struct NoTools;

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
    let initialize = ClientInfo::default().serve(stream);
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
    let client = tokio::time::timeout(budget, ClientInfo::default().serve(stream))
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
