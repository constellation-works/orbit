//! Small wire-level proof for the generic MCP transport kernel.
#![allow(missing_docs)]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_mcp::{ListenerExposure, McpHost, McpListener, OrbitToolServer};
use orbit_types::tool::{
    McpToolAnnotations, McpToolDefinition, McpToolScope, ToolParam, ToolSchema, ToolSessionContext,
};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientInfo, Meta};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio::net::TcpStream;

/// A worker binding must agree with the SSH session machine, and a refused
/// replacement must preserve the original binding and authority reduction.
#[tokio::test]
async fn ssh_worker_initialize_fences_the_execution_machine_and_keeps_its_binding() {
    use orbit_types::task::ExecutionLocation;
    use orbit_types::tool::{McpCapability, McpTransport, WorkerInvocation};
    use tokio::io::{AsyncBufReadExt, BufReader};

    let binding = WorkerInvocation {
        owner_machine_id: "hm_owner".into(),
        owner_workspace_id: "ws_owner".into(),
        owner_destination: "hm_owner/ws_owner".into(),
        task_id: "fixture-task".into(),
        claim_id: "fixture-claim".into(),
        execution: ExecutionLocation {
            machine_id: "hm_follower".into(),
            machine_name: None,
        },
        bound_run_id: "fixture-run".into(),
    };
    for (transport, machine, accepted) in [
        (McpTransport::SshMcp, Some("hm_follower"), true),
        (McpTransport::SshMcp, Some("hm_other"), false),
        (McpTransport::SshMcp, None, false),
        (McpTransport::Local, Some("hm_follower"), false),
    ] {
        let host = Arc::new(EchoHost {
            contexts: Mutex::new(Vec::new()),
            list_calls: Mutex::new(0),
        });
        let session = ToolSessionContext {
            caller_machine_id: machine.map(str::to_owned),
            process_machine_id: Some("hm_owner".into()),
            transport: Some(transport),
            effective_capabilities: [McpCapability::Agent, McpCapability::Operator].into(),
            ..Default::default()
        };
        let server = OrbitToolServer::new_with_context(host.clone(), session);
        let (client_io, server_io) = duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            if let Ok(service) = server.serve(server_io).await {
                service.waiting().await.expect("worker session closes");
            }
        });
        let (client_read, mut client_write) = tokio::io::split(client_io);
        let mut client_read = BufReader::new(client_read);
        let initialize = |id, binding: &WorkerInvocation| {
            json!({
                "jsonrpc": "2.0", "id": id, "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "worker-fixture", "version": "0"},
                    "_meta": {"orbit": {"worker_invocation": binding}}
                }
            })
        };
        let request = |binding: &WorkerInvocation| format!("{}\n", initialize(1, binding));
        client_write
            .write_all(request(&binding).as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client_read.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        if !accepted {
            assert_eq!(response["error"]["code"], -32602, "{response}");
            assert_eq!(
                response["error"]["message"],
                "worker session binding refused"
            );
            assert!(host.contexts.lock().unwrap().is_empty());
        } else {
            assert!(response.get("result").is_some(), "{response}");
            client_write
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                .await
                .unwrap();
            // Identical initialization can be replayed, but neither machine
            // nor attempt may be replaced once the session is bound.
            let mut other_machine = binding.clone();
            other_machine.execution.machine_id = "hm_other".into();
            let mut other_attempt = binding.clone();
            other_attempt.bound_run_id = "other-run".into();
            for (id, proposed, refused) in [
                (2, &binding, false),
                (3, &other_machine, true),
                (4, &other_attempt, true),
            ] {
                client_write
                    .write_all(format!("{}\n", initialize(id, proposed)).as_bytes())
                    .await
                    .unwrap();
                line.clear();
                tokio::time::timeout(
                    std::time::Duration::from_secs(10),
                    client_read.read_line(&mut line),
                )
                .await
                .unwrap()
                .unwrap();
                let response: Value = serde_json::from_str(&line).unwrap();
                if refused {
                    assert_eq!(response["error"]["code"], -32602, "{response}");
                    assert_eq!(
                        response["error"]["message"],
                        "worker session binding refused"
                    );
                } else {
                    assert!(response.get("result").is_some(), "{response}");
                }
            }
            let call = json!({"jsonrpc":"2.0", "id":5, "method":"tools/call", "params":{"name":"demo_echo", "arguments":{"value":"inspect binding"}}});
            client_write
                .write_all(format!("{call}\n").as_bytes())
                .await
                .unwrap();
            line.clear();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                client_read.read_line(&mut line),
            )
            .await
            .unwrap()
            .unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            assert!(response.get("result").is_some(), "{response}");
            let contexts = host.contexts.lock().unwrap();
            assert_eq!(contexts.len(), 1);
            assert_eq!(contexts[0].worker_invocation.as_ref(), Some(&binding));
            assert_eq!(contexts[0].caller_machine_id.as_deref(), machine);
            assert_eq!(
                contexts[0].effective_capabilities,
                [McpCapability::Agent].into()
            );
        }
        drop(client_write);
        drop(client_read);
        tokio::time::timeout(std::time::Duration::from_secs(10), server_task)
            .await
            .unwrap()
            .unwrap();
    }
}

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
            description: "Echo one generic value; inspect it with `demo.inspect`.".to_string(),
            parameters: vec![ToolParam {
                name: "value".to_string(),
                description: "Value to echo, as `demo.inspect` reports it.".to_string(),
                param_type: "string".to_string(),
                required: true,
            }],
            builtin: false,
        },
        McpToolScope::WorkspaceRequired,
    )
    .with_annotations((name == "demo.inspect").then_some(McpToolAnnotations::READ_ONLY))
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

    assert!(initialized.capabilities.resources.is_some());
    let resources = client
        .peer()
        .list_resources(Default::default())
        .await
        .expect("resources/list");
    assert_eq!(resources.resources.len(), 1);
    let uri = &resources.resources[0].uri;
    assert_eq!(uri, "ui://orbit/control-center/v1/index.html");
    let legacy = client
        .peer()
        .read_resource(rmcp::model::ReadResourceRequestParams::new(
            "ui://orbit/task-panel/v1/index.html",
        ))
        .await
        .expect("legacy resource remains reachable");
    let legacy_wire = serde_json::to_value(legacy).expect("legacy resource JSON");
    assert_eq!(
        legacy_wire["contents"][0]["uri"],
        "ui://orbit/task-panel/v1/index.html"
    );
    let resource = client
        .peer()
        .read_resource(rmcp::model::ReadResourceRequestParams::new(uri))
        .await
        .expect("resources/read");
    let wire = serde_json::to_value(&resource).expect("resource JSON");
    assert_eq!(wire["contents"][0]["mimeType"], "text/html;profile=mcp-app");
    assert_eq!(
        wire["contents"][0]["_meta"]["ui"]["csp"]["connectDomains"],
        json!([])
    );
    for refused in [
        "file:///etc/passwd",
        "ui://orbit/task-panel/v1/../secret",
        "ui://orbit/task-panel/v2/index.html",
        "ui://orbit/task-panel/v1/index.html?workspace=other",
        "https://example.invalid/index.html",
    ] {
        assert!(
            client
                .peer()
                .read_resource(rmcp::model::ReadResourceRequestParams::new(refused))
                .await
                .is_err(),
            "only the exact static resource may be read"
        );
    }

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
    assert_eq!(
        tool.description.as_deref(),
        Some("Echo one generic value; inspect it with `demo_inspect`."),
        "prose names other tools by the name tools/list advertises"
    );
    assert_eq!(
        tool.input_schema["properties"]["value"]["description"],
        json!("Value to echo, as `demo_inspect` reports it.")
    );
    assert!(
        tool.annotations.is_none(),
        "an undeclared tool advertises no annotations"
    );
    let inspect = listed
        .tools
        .iter()
        .find(|tool| tool.name.as_ref() == "demo_inspect")
        .expect("inspect tool listed");
    assert_eq!(
        inspect
            .annotations
            .as_ref()
            .and_then(|hints| hints.read_only_hint),
        Some(true)
    );
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

/// A browser can send a simple HTTP POST to loopback. Its body must never
/// reach rmcp, even when it contains a complete MCP handshake and tool call.
#[tokio::test]
async fn loopback_listener_closes_http_before_dispatching_post_body() {
    let host = Arc::new(EchoHost {
        contexts: Mutex::new(Vec::new()),
        list_calls: Mutex::new(0),
    });
    let listener = McpListener::bind(
        "127.0.0.1:0".parse().expect("loopback address"),
        ListenerExposure::LoopbackOnly,
        host.clone() as Arc<dyn McpHost>,
        ToolSessionContext::trusted_local(None, None, None),
    )
    .await
    .expect("bind loopback listener");
    let addr = listener.local_addr().expect("bound address");
    let accepting = tokio::spawn(listener.serve());

    let mut stream = TcpStream::connect(addr).await.expect("connect over TCP");
    let body = concat!(
        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{},\"clientInfo\":{\"name\":\"browser\",\"version\":\"0\"}}\n",
        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"demo_echo\",\"arguments\":{\"value\":\"unsafe\"}}}\n"
    );
    let request = format!(
        "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("send browser-style POST");
    let mut response = [0; 1];
    let closed = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        stream.read(&mut response),
    )
    .await
    .expect("listener must close HTTP promptly");
    assert!(
        matches!(closed, Ok(0) | Err(_)),
        "HTTP must be closed without a JSON-RPC response: {closed:?}"
    );
    assert_eq!(*host.list_calls.lock().expect("list calls"), 0);
    assert!(host.contexts.lock().expect("tool calls").is_empty());

    accepting.abort();
}

fn call(name: &str, args: Value) -> CallToolRequestParams {
    let arguments: Map<String, Value> = args
        .as_object()
        .expect("tool arguments are an object")
        .clone();
    CallToolRequestParams::new(name.to_string()).with_arguments(arguments)
}

struct TaskPanelHost {
    calls: Mutex<Vec<(String, Value, ToolSessionContext)>>,
    hidden: std::sync::atomic::AtomicBool,
}

impl McpHost for TaskPanelHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        let mut task = definition("orbit.task.show");
        task.annotations = Some(McpToolAnnotations::READ_ONLY);
        Ok(vec![task])
    }
    fn hidden_tool_names(&self, _: &ToolSessionContext) -> std::collections::BTreeSet<String> {
        if self.hidden.load(std::sync::atomic::Ordering::Relaxed) {
            std::collections::BTreeSet::from(["orbit.task.show".to_owned()])
        } else {
            std::collections::BTreeSet::new()
        }
    }
    fn call_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .expect("calls")
            .push((name.to_owned(), input.clone(), context));
        if input["workspace"] != "selected" {
            return Err(OrbitError::UnknownSelector(input["workspace"].to_string()));
        }
        Ok(
            json!({"id": "TST-1", "title": "<script>untrusted</script>", "updated_at": "2026-10-03T00:00:00Z"}),
        )
    }
}

#[tokio::test]
async fn presentation_wire_preserves_dispatch_context_and_refuses_hidden_or_mismatched_reads() {
    let host = Arc::new(TaskPanelHost {
        calls: Mutex::new(Vec::new()),
        hidden: std::sync::atomic::AtomicBool::new(false),
    });
    let trusted = ToolSessionContext::default();
    let server = OrbitToolServer::new_with_context(host.clone(), trusted.clone());
    let (client_io, server_io) = duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (server_read, server_write) = tokio::io::split(server_io);
    let server_task = tokio::spawn(async move {
        server
            .serve((server_read, server_write))
            .await
            .expect("server")
            .waiting()
            .await
            .expect("wait");
    });
    let mut info = ClientInfo::default();
    info.meta = Some(Meta(json!({"orbit":{"workspace":"session-default","actor":"human","effective_capabilities":["operator"]}, "ui":{"authority":"human"}}).as_object().unwrap().clone()));
    let client = info
        .serve((client_read, client_write))
        .await
        .expect("client");
    let listed = client.peer().list_tools(None).await.expect("tools");
    assert_eq!(listed.tools.len(), 3);
    let selected = client
        .peer()
        .call_tool(call(
            "orbit_ui_inspect",
            json!({"workspace":"selected","id":"TST-1"}),
        ))
        .await
        .expect("inspect");
    assert_eq!(selected.is_error, Some(false));
    assert_eq!(
        selected.structured_content.as_ref().unwrap()["workspace"],
        "selected"
    );
    {
        let calls = host.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "orbit.task.show");
        assert_eq!(calls[0].1["workspace"], "selected");
        assert_eq!(
            calls[0].1["fields"],
            json!([
                "id",
                "title",
                "status",
                "updated_at",
                "description",
                "acceptance_criteria"
            ])
        );
        assert_eq!(
            calls[0].2.effective_capabilities,
            trusted.effective_capabilities
        );
        assert!(calls[0].2.trace_id.is_some());
        assert_eq!(calls[0].2.workspace.as_deref(), Some("session-default"));
    }
    let missing = client
        .peer()
        .call_tool(call("orbit_ui_inspect", json!({"id":"TST-1"})))
        .await
        .unwrap();
    assert_eq!(missing.is_error, Some(true));
    assert_eq!(
        host.calls.lock().unwrap().len(),
        1,
        "missing selection never reaches dispatch"
    );
    let wrong = client
        .peer()
        .call_tool(call(
            "orbit_ui_inspect",
            json!({"workspace":"wrong","id":"TST-1"}),
        ))
        .await
        .unwrap();
    assert_eq!(
        wrong.structured_content.as_ref().unwrap()["code"],
        "unknown_selector"
    );
    let mismatch = client
        .peer()
        .call_tool(call(
            "orbit_ui_inspect",
            json!({"workspace":"selected","id":"TST-2"}),
        ))
        .await
        .unwrap();
    assert_eq!(mismatch.is_error, Some(true));
    host.hidden
        .store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(
        client
            .peer()
            .list_tools(None)
            .await
            .unwrap()
            .tools
            .is_empty()
    );
    let hidden = client
        .peer()
        .call_tool(call(
            "orbit_ui_inspect",
            json!({"workspace":"selected","id":"TST-1"}),
        ))
        .await
        .unwrap();
    assert_eq!(hidden.is_error, Some(true));
    assert_eq!(
        host.calls.lock().unwrap().len(),
        3,
        "hidden presentation cannot call the data reader"
    );
    server_task.abort();
}

struct PresentationCollisionHost {
    name: &'static str,
    calls: std::sync::atomic::AtomicUsize,
}

impl McpHost for PresentationCollisionHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        Ok(vec![definition(self.name)])
    }
    fn call_tool(&self, _: &str, _: Value, _: ToolSessionContext) -> Result<Value, OrbitError> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(json!({}))
    }
}

#[tokio::test]
async fn mixed_dot_underscore_names_cannot_shadow_presentation_entrypoints() {
    for name in [
        "orbit.ui_open",
        "orbit_ui.open",
        "orbit.ui_inspect",
        "orbit_ui.inspect",
    ] {
        let host = Arc::new(PresentationCollisionHost {
            name,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let server = OrbitToolServer::new(host.clone());
        let (client_io, server_io) = duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client_io);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server_task = tokio::spawn(async move {
            server
                .serve((server_read, server_write))
                .await
                .expect("server")
                .waiting()
                .await
                .expect("wait");
        });
        let client = ClientInfo::default()
            .serve((client_read, client_write))
            .await
            .expect("client");
        assert!(
            client.peer().list_tools(None).await.is_err(),
            "reserved advertised names refuse rather than duplicate or shadow"
        );
        let result = client
            .peer()
            .call_tool(call(
                &orbit_types::tool::mcp_advertised_tool_name(name),
                json!({"workspace":"selected","id":"TST-1"}),
            ))
            .await
            .unwrap();
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            host.calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "collision refusal happens before host dispatch"
        );
        server_task.abort();
    }
}

/// Simulates selector translation by the real workspace/federation host.
struct DesktopWireHost {
    calls: Mutex<Vec<(String, Value, ToolSessionContext)>>,
}
impl McpHost for DesktopWireHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        let mut task_show = definition("orbit.task.show");
        task_show.annotations = Some(McpToolAnnotations::READ_ONLY);
        let mut definitions = vec![
            task_show,
            definition("orbit.task.list"),
            definition("orbit.task.update"),
            definition("orbit.workflow.run.show"),
        ];
        for definition in &mut definitions {
            definition.schema.builtin = true;
            if definition.schema.name == "orbit.workflow.run.show" {
                definition
                    .schema
                    .parameters
                    .push(orbit_types::tool::ToolParam {
                        name: "view".into(),
                        param_type: "string".into(),
                        required: false,
                        description: "Bounded observation".into(),
                    });
            }
        }
        Ok(definitions)
    }
    fn call_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .unwrap()
            .push((name.into(), input.clone(), context));
        if input["workspace"] != "host-qualified:opaque-workspace" {
            return Err(OrbitError::UnknownSelector(
                "unknown desktop destination".into(),
            ));
        }
        Ok(
            json!({"workspace":"/owner/local/checkout","schema_version":1,"run":{"run_id":"jrun-proof"},"snapshot":{"revision":"revision-1","task":{"id":"TST-1"}}}),
        )
    }
}

#[tokio::test]
async fn desktop_wire_preserves_explicit_destination_and_run_inspector_authority() {
    let host = Arc::new(DesktopWireHost {
        calls: Mutex::new(Vec::new()),
    });
    let trusted = ToolSessionContext::default();
    let server = OrbitToolServer::new_with_context(host.clone(), trusted.clone());
    let (client_io, server_io) = duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        server
            .serve(tokio::io::split(server_io))
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let mut info = ClientInfo::default();
    info.meta = Some(Meta(
        json!({"orbit":{"workspace":"session-default","effective_capabilities":["operator"]}})
            .as_object()
            .unwrap()
            .clone(),
    ));
    let client = info.serve(tokio::io::split(client_io)).await.unwrap();
    for (name, arguments, missing_arguments) in [
        (
            "orbit_task_list",
            json!({"workspace":"host-qualified:opaque-workspace","view":"bounded"}),
            json!({"view":"bounded"}),
        ),
        (
            "orbit_task_show",
            json!({"workspace":"host-qualified:opaque-workspace","id":"TST-1","snapshot":true}),
            json!({"id":"TST-1","snapshot":true}),
        ),
        (
            "orbit_task_update",
            json!({"workspace":"host-qualified:opaque-workspace","id":"TST-1","request_id":"wire-proof","expected_revision":"seen","comment":"proof"}),
            json!({"id":"TST-1","request_id":"wire-proof","expected_revision":"seen","comment":"proof"}),
        ),
    ] {
        let response = client
            .peer()
            .call_tool(call(name, arguments))
            .await
            .unwrap();
        assert_eq!(response.is_error, Some(false), "{response:?}");
        assert_eq!(
            response.structured_content.unwrap()["workspace"],
            "host-qualified:opaque-workspace",
            "opaque route survives owner path translation"
        );
        let missing = client
            .peer()
            .call_tool(call(name, missing_arguments))
            .await
            .unwrap();
        assert_eq!(
            missing.is_error,
            Some(true),
            "session default never substitutes for explicit desktop selection"
        );
    }
    let opened = client
        .peer()
        .call_tool(call(
            "orbit_ui_open",
            json!({"workspace":"host-qualified:opaque-workspace"}),
        ))
        .await
        .unwrap();
    assert_eq!(
        opened.structured_content.unwrap()["workspace"],
        "host-qualified:opaque-workspace"
    );
    assert_eq!(
        host.calls.lock().unwrap().len(),
        3,
        "opening a selector shell performs no default read"
    );
    let inspected = client
        .peer()
        .call_tool(call(
            "orbit_ui_inspect",
            json!({"workspace":"host-qualified:opaque-workspace","id":"jrun-proof","kind":"run"}),
        ))
        .await
        .unwrap();
    assert_eq!(inspected.is_error, Some(false));
    assert_eq!(
        inspected.structured_content.unwrap()["workspace"],
        "host-qualified:opaque-workspace"
    );
    {
        let calls = host.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[3].0, "orbit.workflow.run.show");
        assert_eq!(calls[3].1["view"], "bounded");
        assert_eq!(
            calls[3].2.effective_capabilities, trusted.effective_capabilities,
            "UI claims cannot promote session authority"
        );
        assert!(calls[3].2.trace_id.is_some());
    }
    let mismatch = client
        .peer()
        .call_tool(call(
            "orbit_ui_inspect",
            json!({"workspace":"host-qualified:opaque-workspace","id":"jrun-other","kind":"run"}),
        ))
        .await
        .unwrap();
    assert_eq!(mismatch.is_error, Some(true));
    client.cancel().await.unwrap();
    server_task.await.unwrap();
}

struct CanonicalContractHost;
impl McpHost for CanonicalContractHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        orbit_mcp::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))
    }
    fn call_tool(
        &self,
        name: &str,
        input: Value,
        _: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Ok(json!({"name":name,"input":input}))
    }
}

#[tokio::test]
async fn client_handshake_does_not_advertise_retired_desktop_tools() {
    for legacy in [true, false] {
        let server = OrbitToolServer::new(Arc::new(CanonicalContractHost));
        let (client_io, server_io) = duplex(128 * 1024);
        let server_task = tokio::spawn(async move {
            let service = server.serve(server_io).await.unwrap();
            service.waiting().await.unwrap();
        });
        let mut info = ClientInfo::default();
        info.client_info.name = "orbit-federated-mux".into();
        if !legacy {
            info.meta = Some(Meta(
                json!({"orbit":{"domain_contract":1}})
                    .as_object()
                    .unwrap()
                    .clone(),
            ));
        }
        let client = info.serve(client_io).await.unwrap();
        let listed = client.peer().list_all_tools().await.unwrap();
        assert_eq!(
            listed
                .iter()
                .filter(|tool| tool.name.starts_with("orbit_desktop_"))
                .count(),
            0
        );
        let call = client
            .peer()
            .call_tool(
                CallToolRequestParams::new("orbit_task_show").with_arguments(
                    json!({"workspace":"ws_fixture","id":"TST-1","snapshot":true})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
            )
            .await
            .unwrap();
        let value = call.structured_content.unwrap();
        assert_eq!(value["name"], "orbit.task.show");
        assert_eq!(value["input"]["snapshot"], true);
        client.cancel().await.unwrap();
        server_task.await.unwrap();
    }
}

/// An owner whose task read timed out waiting on a store lock.
struct LockBusyHost;
impl McpHost for LockBusyHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        Ok(vec![definition("demo.echo")])
    }
    fn call_tool(&self, _: &str, _: Value, _: ToolSessionContext) -> Result<Value, OrbitError> {
        Err(OrbitError::FileLockTimeout(Box::new(
            orbit_common::fs::io::FileLockTimeout {
                lock_path: "/state/tasks/workspaces/ws/.task-commit.lock".into(),
                label: "task commit boundary: recovery at src/task.rs:1".into(),
                timeout_ms: 30_000,
                holder: None,
                shared_holders: Vec::new(),
            },
        )))
    }
}

/// [ORB-15088] A call that lost only to a lock-wait deadline reaches its
/// caller as the retryable `lock_busy`, not `internal_error`: a pull follower
/// reads that code off the wire to retry its owner read and to keep the
/// failure from settling as a fault of its candidate.
#[tokio::test]
async fn a_lock_wait_timeout_is_a_retryable_lock_busy_on_the_wire() {
    let server = OrbitToolServer::new_with_context(
        Arc::new(LockBusyHost),
        ToolSessionContext::trusted_local(None, None, None),
    );
    let (client_io, server_io) = duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        let service = server.serve(server_io).await.expect("serve");
        service.waiting().await.expect("wait");
    });
    let mut info = ClientInfo::default();
    info.meta = Some(Meta(
        json!({ "orbit": { "workspace": "/tmp/owner" } })
            .as_object()
            .expect("initialize metadata")
            .clone(),
    ));
    let client = info.serve(client_io).await.expect("client");

    let busy = client
        .peer()
        .call_tool(call("demo_echo", json!({ "value": "ORB-1" })))
        .await
        .expect("a structured tool error");
    assert_eq!(busy.is_error, Some(true));
    let payload = busy.structured_content.expect("structured error");
    assert_eq!(
        payload["code"],
        orbit_common::LOCK_BUSY_ERROR_CODE,
        "{payload}"
    );
    assert!(
        payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("timed out after 30000ms")),
        "{payload}"
    );

    client.cancel().await.expect("cancel");
    server_task.await.expect("server");
}
