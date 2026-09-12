//! Conformance tests for schema-adjacent builtin MCP definitions.
#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use orbit_common::OrbitError;
use orbit_tools::{Tool, ToolContext, ToolRegistry, canonical_builtin_mcp_tool_definitions};
use orbit_types::tool::{McpToolDefinitionError, McpToolScope, ToolSchema};
use serde_json::Value;

struct TestTool(&'static str);

impl Tool for TestTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.0.to_string(),
            description: "test tool".to_string(),
            parameters: Vec::new(),
            builtin: true,
        }
    }

    fn execute(&self, _ctx: &ToolContext, _input: Value) -> Result<Value, OrbitError> {
        Ok(Value::Null)
    }
}

#[test]
fn canonical_builtin_definitions_preserve_the_exact_workspace_surface() {
    let definitions =
        canonical_builtin_mcp_tool_definitions().expect("builtin MCP definitions are valid");
    assert_eq!(
        definitions
            .iter()
            .map(|definition| definition.schema.name.as_str())
            .collect::<Vec<_>>(),
        [
            "orbit.agent.invoke",
            "orbit.auto_task.list",
            "orbit.auto_task.mint",
            "orbit.command.exec",
            "orbit.friction.add",
            "orbit.friction.list",
            "orbit.friction.update",
            "orbit.search",
            "orbit.task.add",
            "orbit.task.artifact.get",
            "orbit.task.artifact.put",
            "orbit.task.list",
            "orbit.task.show",
            "orbit.task.update",
            "orbit.workflow.run.list",
            "orbit.workflow.run.resume",
            "orbit.workflow.run.show",
            "orbit.workflow.run.workers",
            "orbit.workflow.ship",
        ]
    );
    assert!(
        definitions
            .iter()
            .all(|definition| definition.schema.builtin)
    );
    assert!(
        definitions
            .iter()
            .all(|definition| definition.scope == McpToolScope::WorkspaceRequired)
    );
    assert!(
        definitions.iter().all(|definition| {
            !matches!(definition.schema.name.as_str(), "orbit.workspace.list")
        })
    );
}

#[test]
fn ordinary_registration_stays_off_the_mcp_surface() {
    let mut missing = ToolRegistry::new();
    missing.register(TestTool("demo.missing"));
    assert!(
        missing
            .mcp_tool_definitions()
            .expect("unclassified tools are valid but unexposed")
            .is_empty()
    );
}

#[test]
fn invalid_and_duplicate_names_fail_closed() {
    let mut invalid = ToolRegistry::new();
    invalid.register_mcp(TestTool(" "), McpToolScope::WorkspaceRequired);
    assert_eq!(
        invalid.mcp_tool_definitions(),
        Err(McpToolDefinitionError::EmptyCanonicalName)
    );

    let mut canonical = ToolRegistry::new();
    canonical.register_mcp(TestTool("demo.same"), McpToolScope::WorkspaceRequired);
    canonical.register_mcp(TestTool("demo.same"), McpToolScope::WorkspaceRequired);
    assert!(matches!(
        canonical.mcp_tool_definitions(),
        Err(McpToolDefinitionError::DuplicateCanonicalName(_))
    ));

    let mut advertised = ToolRegistry::new();
    advertised.register_mcp(TestTool("demo.name"), McpToolScope::WorkspaceRequired);
    advertised.register_mcp(TestTool("demo_name"), McpToolScope::WorkspaceRequired);
    assert!(matches!(
        advertised.mcp_tool_definitions(),
        Err(McpToolDefinitionError::DuplicateAdvertisedName(_))
    ));
}
