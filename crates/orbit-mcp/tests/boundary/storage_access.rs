//! Native store permission failures retain their cause at the MCP wire boundary.
use orbit_common::{OrbitError, StorageLayer};
use orbit_mcp::{McpHost, OrbitToolServer};
use orbit_types::tool::{McpToolDefinition, McpToolScope, ToolSchema, ToolSessionContext};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use serde_json::Value;
use std::sync::Arc;
use tokio::io::duplex;

struct AccessDeniedHost;
impl McpHost for AccessDeniedHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        Ok(vec![McpToolDefinition::new(
            ToolSchema {
                name: "permission.probe".into(),
                description: "Read workspace state".into(),
                parameters: vec![],
                builtin: false,
            },
            McpToolScope::WorkspaceRequired,
        )])
    }
    fn call_tool(&self, _: &str, _: Value, _: ToolSessionContext) -> Result<Value, OrbitError> {
        Err(OrbitError::storage_io(
            StorageLayer::Migration,
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            "cannot read workspace layout marker",
        ))
    }
}

#[tokio::test]
async fn a_store_permission_denial_is_not_a_migration_failure_on_the_wire() {
    let server = OrbitToolServer::new_with_context(
        Arc::new(AccessDeniedHost),
        ToolSessionContext::trusted_local(Some("/fixture/work".into()), None, None),
    );
    let (client_io, server_io) = duplex(64 * 1024);
    let server_task = tokio::spawn(async move {
        let service = server.serve(server_io).await.unwrap();
        service.waiting().await.unwrap();
    });
    let client = ClientConfig::default().serve(client_io).await.unwrap();
    let response = client
        .peer()
        .call_tool(CallToolRequestParams::new("permission_probe"))
        .await
        .unwrap();
    assert_eq!(response.is_error, Some(true));
    let payload = response.structured_content.unwrap();
    assert_eq!(payload["code"], "storage_access_denied", "{payload}");
    client.cancel().await.unwrap();
    server_task.await.unwrap();
}
