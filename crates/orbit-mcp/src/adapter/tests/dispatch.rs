use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_types::tool::{McpToolDefinition, McpToolScope, ToolSessionContext};
use rmcp::model::CallToolRequestParams;
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::OrbitToolServer;
use super::super::name_map::sanitize_tool_name;
use super::super::test_support::{EchoArrayHost, StubHost, tool_schema};

struct CompleteSurfaceHost {
    calls: Mutex<Vec<(String, ToolSessionContext)>>,
}

impl CompleteSurfaceHost {
    fn definition(name: &str) -> McpToolDefinition {
        McpToolDefinition::new(tool_schema(name), McpToolScope::WorkspaceRequired)
    }
}

impl crate::McpHost for CompleteSurfaceHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        Ok(vec![
            Self::definition("demo.read"),
            Self::definition("demo.write"),
            Self::definition("demo.inspect"),
        ])
    }

    fn call_tool(
        &self,
        name: &str,
        _input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.calls
            .lock()
            .expect("calls lock")
            .push((name.to_string(), context));
        Ok(json!({ "tool": name }))
    }
}

struct InvalidNameHost;

impl crate::McpHost for InvalidNameHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        Ok(vec![CompleteSurfaceHost::definition("")])
    }

    fn call_tool(
        &self,
        _name: &str,
        _input: Value,
        _context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Ok(Value::Null)
    }
}

#[tokio::test]
async fn complete_definition_set_is_listed_and_callable() {
    let host = Arc::new(CompleteSurfaceHost {
        calls: Mutex::new(Vec::new()),
    });
    let server = OrbitToolServer::new_with_context(host.clone(), ToolSessionContext::default());

    let names = server
        .tool_schemas()
        .expect("complete tool list")
        .into_iter()
        .map(|schema| schema.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["demo.read", "demo.write", "demo.inspect"]);

    for name in ["demo_read", "demo_write", "demo_inspect"] {
        let result = server
            .call_tool_request(CallToolRequestParams::new(name))
            .await
            .expect("every listed tool reaches the host");
        assert_eq!(result.is_error, Some(false));
    }

    let calls = host.calls.lock().expect("calls lock");
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].0, "demo.read");
    assert_eq!(calls[1].0, "demo.write");
    assert_eq!(calls[2].0, "demo.inspect");
}

#[tokio::test]
async fn every_tool_call_gets_one_fresh_trace_without_rewriting_legacy_call_id() {
    let host = Arc::new(CompleteSurfaceHost {
        calls: Mutex::new(Vec::new()),
    });
    let trusted = ToolSessionContext {
        mcp_call_id: Some("legacy-call".to_string()),
        ..ToolSessionContext::default()
    };
    let server = OrbitToolServer::new_with_context(host.clone(), trusted);

    for _ in 0..2 {
        server
            .call_tool_request(CallToolRequestParams::new("demo_read"))
            .await
            .expect("call succeeds");
    }

    let calls = host.calls.lock().expect("calls lock");
    let first = &calls[0].1;
    let second = &calls[1].1;
    assert!(
        first
            .trace_id
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert!(
        second
            .trace_id
            .as_deref()
            .is_some_and(|id| id.starts_with("trace-"))
    );
    assert_ne!(first.trace_id, second.trace_id);
    assert_eq!(first.mcp_call_id.as_deref(), Some("legacy-call"));
    assert_eq!(second.mcp_call_id.as_deref(), Some("legacy-call"));
}

#[test]
fn invalid_canonical_names_fail_the_surface_before_dispatch() {
    let server = OrbitToolServer::new(Arc::new(InvalidNameHost));
    let error = server
        .tool_schemas()
        .expect_err("empty canonical name is invalid");
    assert!(error.to_string().contains("must not be empty"));
}

#[tokio::test]
async fn array_results_are_object_shaped_for_strict_mcp_clients() {
    let affected_tools = ["orbit.task.list", "orbit.friction.list"];
    let host = Arc::new(EchoArrayHost {
        schemas: affected_tools
            .iter()
            .map(|name| tool_schema(name))
            .collect(),
    });
    let server = OrbitToolServer::new(host);

    for canonical_name in affected_tools {
        let result = server
            .call_tool_request(CallToolRequestParams::new(sanitize_tool_name(
                canonical_name,
            )))
            .await
            .expect("MCP call succeeds");
        let structured = result
            .structured_content
            .as_ref()
            .expect("structured content");
        assert_eq!(
            structured.get("items"),
            Some(&json!([{ "tool": canonical_name }]))
        );
        let wire = serde_json::to_value(&result).expect("serialize result");
        assert!(
            wire.get("structuredContent").is_some_and(Value::is_object),
            "{canonical_name} must satisfy object-only clients"
        );
    }
}

#[test]
fn canonical_name_translates_advertised_back_to_dotted() {
    let server = OrbitToolServer::new(Arc::new(StubHost {
        schemas: vec![tool_schema("orbit.task.add")],
    }));
    assert_eq!(
        server.canonical_name("orbit_task_add").unwrap(),
        "orbit.task.add"
    );
    assert_eq!(
        server.canonical_name("orbit_task_add").unwrap(),
        "orbit.task.add"
    );
}

#[test]
fn canonical_name_passes_unknown_and_legacy_dotted_names_to_the_host() {
    let server = OrbitToolServer::new(Arc::new(StubHost {
        schemas: vec![tool_schema("orbit.task.add")],
    }));
    assert_eq!(
        server.canonical_name("orbit.task.add").unwrap(),
        "orbit.task.add"
    );
    assert_eq!(
        server.canonical_name("totally.unknown").unwrap(),
        "totally.unknown"
    );
}

#[test]
fn canonical_name_rejects_sanitized_dispatch_collisions() {
    let server = OrbitToolServer::new(Arc::new(StubHost {
        schemas: vec![tool_schema("foo.bar"), tool_schema("foo_bar")],
    }));
    let error = server
        .canonical_name("foo_bar")
        .expect_err("dispatch must reject ambiguous advertised names");
    assert!(
        error
            .message
            .contains("invalid canonical MCP tool definitions")
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredWorkspaceInput {
    workspace: String,
}

struct StrictPluginHost {
    federated: bool,
}

impl crate::McpHost for StrictPluginHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        let mut version = tool_schema("orbit.graph.version");
        version.builtin = false;
        let mut declared = tool_schema("orbit.graph.declared");
        declared.builtin = false;
        declared
            .parameters
            .push(super::super::test_support::param("workspace"));
        Ok(vec![
            McpToolDefinition::new(version, McpToolScope::WorkspaceRequired),
            McpToolDefinition::new(declared, McpToolScope::WorkspaceRequired),
        ])
    }

    fn federated_workspace_selectors(&self) -> bool {
        self.federated
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let selector = input
            .get("workspace")
            .and_then(Value::as_str)
            .or(context.workspace.as_deref())
            .ok_or_else(|| OrbitError::InvalidInput("missing routed workspace".to_string()))?
            .to_string();
        match name {
            "orbit.graph.version" => {
                serde_json::from_value::<VersionInput>(input)
                    .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
                Ok(json!({ "routed_workspace": selector }))
            }
            "orbit.graph.declared" => {
                let declared = serde_json::from_value::<DeclaredWorkspaceInput>(input)
                    .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
                Ok(json!({ "routed_workspace": selector, "plugin_workspace": declared.workspace }))
            }
            _ => Err(OrbitError::InvalidInput("unknown fixture tool".to_string())),
        }
    }
}

#[tokio::test]
async fn plugin_selector_routes_without_leaking_into_strict_plugin_input() {
    for federated in [false, true] {
        let server = OrbitToolServer::new(Arc::new(StrictPluginHost { federated }));
        let definition = server
            .tool_definitions()
            .expect("fixture definitions")
            .into_iter()
            .find(|definition| definition.schema.name == "orbit.graph.version")
            .expect("version tool");
        let advertised = server
            .input_schema_for(&definition)
            .expect("plugin input schema");
        assert_eq!(advertised["required"], json!(["workspace"]));
        let description = advertised["properties"]["workspace"]["description"]
            .as_str()
            .expect("injected selector description");
        assert_eq!(description.contains("Copy the `selector` field"), federated);
        let result = server
            .call_tool_request(super::super::test_support::request_with_args(
                "orbit.graph.version",
                json!({ "workspace": "ws_orbit" }),
            ))
            .await
            .expect("MCP dispatch completes");
        assert_eq!(result.is_error, Some(false), "federated={federated}");
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .map(|value| &value["routed_workspace"]),
            Some(&json!("ws_orbit")),
            "federated={federated}"
        );

        let declared = server
            .call_tool_request(super::super::test_support::request_with_args(
                "orbit.graph.declared",
                json!({ "workspace": "ws_orbit" }),
            ))
            .await
            .expect("MCP dispatch completes");
        assert_eq!(declared.is_error, Some(false));
        assert_eq!(
            declared
                .structured_content
                .as_ref()
                .map(|value| &value["plugin_workspace"]),
            Some(&json!("ws_orbit"))
        );
    }
}
