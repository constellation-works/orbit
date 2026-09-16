//! Small wire-level proof for the generic MCP transport kernel.
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_mcp::{ListenerExposure, McpHost, McpListener, OrbitToolServer};
use orbit_types::tool::{
    McpToolDefinition, McpToolScope, ToolParam, ToolSchema, ToolSessionContext,
};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientInfo, Meta};
use serde_json::{Map, Value, json};
use tokio::io::duplex;
use tokio::net::TcpStream;

struct EchoHost {
    contexts: Mutex<Vec<ToolSessionContext>>,
    list_calls: Mutex<usize>,
}

impl McpHost for EchoHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        *self.list_calls.lock().expect("list calls") += 1;
        Ok(vec![definition("demo.echo"), definition("demo.inspect")])
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if name != "demo.echo" && name != "demo.inspect" {
            return Err(OrbitError::not_found(
                orbit_common::NotFoundKind::Tool,
                name.to_string(),
            ));
        }
        self.contexts
            .lock()
            .expect("contexts")
            .push(context.clone());
        Ok(json!({
            "tool": name,
            "echo": input.get("value"),
            "workspace": context.workspace,
        }))
    }
}

fn definition(name: &str) -> McpToolDefinition {
    McpToolDefinition::new(
        ToolSchema {
            name: name.to_string(),
            description: "Echo one generic value.".to_string(),
            parameters: vec![ToolParam {
                name: "value".to_string(),
                description: "Value to echo.".to_string(),
                param_type: "string".to_string(),
                required: true,
            }],
            builtin: false,
        },
        McpToolScope::WorkspaceRequired,
    )
}

#[tokio::test]
async fn generic_kernel_round_trips_initialize_list_call_and_error() {
    let host = Arc::new(EchoHost {
        contexts: Mutex::new(Vec::new()),
        list_calls: Mutex::new(0),
    });
    let server_host: Arc<dyn McpHost> = host.clone();
    let trusted = ToolSessionContext::trusted_local(None, None, None);
    let server = OrbitToolServer::new_with_context(server_host, trusted);

    let (client_io, server_io) = duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, server_write) = tokio::io::split(server_io);
    let server_task = tokio::spawn(async move {
        let service = server
            .serve((server_read, server_write))
            .await
            .expect("serve MCP fixture");
        service.waiting().await.expect("wait for MCP fixture");
    });

    let mut client_info = ClientInfo::default();
    client_info.meta = Some(Meta(
        json!({ "orbit": { "workspace": "/tmp/generic-workspace" } })
            .as_object()
            .expect("initialize metadata")
            .clone(),
    ));
    let client = client_info
        .serve((client_read, client_write))
        .await
        .expect("connect MCP fixture");

    let initialized = client.peer_info().expect("initialize result");
    assert_eq!(initialized.server_info.name, "orbit-mcp");
    assert!(
        initialized
            .instructions
            .as_deref()
            .is_some_and(|instructions| instructions.contains("tools/list"))
    );

    let listed = client
        .peer()
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    let listed_again = client
        .peer()
        .list_tools(Default::default())
        .await
        .expect("cached tools/list");
    assert_eq!(listed.tools.len(), 2);
    assert_eq!(listed_again, listed);
    let tool = listed
        .tools
        .iter()
        .find(|tool| tool.name.as_ref() == "demo_echo")
        .expect("agent-tagged tool listed");
    assert_eq!(tool.name.as_ref(), "demo_echo");
    assert_eq!(tool.input_schema["required"], json!(["value"]));
    assert!(
        tool.input_schema["properties"]["value"]
            .get("enum")
            .is_none()
    );
    assert!(
        listed
            .tools
            .iter()
            .any(|tool| tool.name.as_ref() == "demo_inspect"),
        "operator-tagged definitions remain on the complete surface"
    );

    let result = client
        .peer()
        .call_tool(call("demo_inspect", json!({ "value": "hello" })))
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.structured_content,
        Some(json!({
            "tool": "demo.inspect",
            "echo": "hello",
            "workspace": "/tmp/generic-workspace",
        }))
    );

    let missing = client
        .peer()
        .call_tool(call("demo_missing", json!({})))
        .await
        .expect("unknown tools/call returns a structured error");
    assert_eq!(missing.is_error, Some(true));
    assert_eq!(
        missing.structured_content.as_ref().unwrap()["code"],
        "tool_not_found"
    );

    let contexts = host.contexts.lock().expect("contexts");
    assert_eq!(contexts.len(), 1);
    assert_eq!(
        contexts[0].workspace.as_deref(),
        Some("/tmp/generic-workspace")
    );
    assert!(contexts[0].trace_id.is_some());
    assert_eq!(*host.list_calls.lock().expect("list calls"), 1);

    server_task.abort();
}

/// The listener transport end to end: bind loopback, complete an
/// initialize/list/call round trip over a real socket, and prove the accepted
/// peer's IP reached the host's audit context — then take the listener down and
/// show the socket is gone.
#[tokio::test]
async fn loopback_listener_round_trips_a_session_and_records_the_peer_ip() {
    let host = Arc::new(EchoHost {
        contexts: Mutex::new(Vec::new()),
        list_calls: Mutex::new(0),
    });
    let listener = McpListener::bind(
        "127.0.0.1:0"
            .parse::<SocketAddr>()
            .expect("loopback address"),
        ListenerExposure::LoopbackOnly,
        host.clone() as Arc<dyn McpHost>,
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind loopback listener");
    let addr = listener.local_addr().expect("bound address");
    assert_ne!(addr.port(), 0, "the kernel-assigned port must be readable");
    let accepting = tokio::spawn(listener.serve());

    let stream = TcpStream::connect(addr).await.expect("connect over TCP");
    let (client_read, client_write) = tokio::io::split(stream);
    let mut client_info = ClientInfo::default();
    client_info.meta = Some(Meta(
        json!({ "orbit": { "workspace": "/tmp/listener-workspace" } })
            .as_object()
            .expect("initialize metadata")
            .clone(),
    ));
    let client = client_info
        .serve((client_read, client_write))
        .await
        .expect("initialize over the listener");
    assert_eq!(
        client
            .peer_info()
            .expect("initialize result")
            .server_info
            .name,
        "orbit-mcp"
    );

    let listed = client
        .peer()
        .list_tools(Default::default())
        .await
        .expect("tools/list");
    assert!(
        listed
            .tools
            .iter()
            .any(|tool| tool.name.as_ref() == "demo_echo"),
        "listener serves the same surface as stdio: {:?}",
        listed.tools
    );

    let result = client
        .peer()
        .call_tool(call("demo_echo", json!({ "value": "over-tcp" })))
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(false));
    assert_eq!(
        result.structured_content,
        Some(json!({
            "tool": "demo.echo",
            "echo": "over-tcp",
            "workspace": "/tmp/listener-workspace",
        }))
    );

    {
        let contexts = host.contexts.lock().expect("contexts");
        assert_eq!(contexts.len(), 1, "one host call per tools/call");
        assert_eq!(
            contexts[0].caller_ip.as_deref(),
            Some("127.0.0.1"),
            "the accepted peer's IP must reach the audit context"
        );
        assert!(
            contexts[0].origin_session_id.is_some(),
            "each session mints its own origin id"
        );
        assert!(contexts[0].trace_id.is_some());
    }

    client.cancel().await.expect("close the MCP session");
    accepting.abort();
    assert!(
        accepting
            .await
            .expect_err("the accept loop is cancelled, never resolved")
            .is_cancelled()
    );
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "the listening socket must be closed once the accept task is gone"
    );
}

fn call(name: &str, args: Value) -> CallToolRequestParams {
    let arguments: Map<String, Value> = args
        .as_object()
        .expect("tool arguments are an object")
        .clone();
    CallToolRequestParams::new(name.to_string()).with_arguments(arguments)
}
