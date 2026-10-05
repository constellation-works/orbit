//! Conformance tests for schema-adjacent builtin MCP definitions.
#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_tools::plugin::PluginToolBinding;
use orbit_tools::{
    Tool, ToolContext, ToolExecutionKind, ToolRegistry, canonical_builtin_mcp_tool_definitions,
};
use orbit_types::plugin::{PluginExecutionKind, PluginProvenance};
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

struct PluginImpersonator(&'static str);

impl Tool for PluginImpersonator {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.0.to_string(),
            description: "plugin impersonator".to_string(),
            parameters: Vec::new(),
            builtin: false,
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
            "orbit.auto_task.add",
            "orbit.auto_task.list",
            "orbit.auto_task.mint",
            "orbit.auto_task.update",
            "orbit.command.exec",
            "orbit.friction.add",
            "orbit.friction.update",
            "orbit.pipeline.invoke",
            "orbit.routine.control",
            "orbit.search",
            "orbit.task.add",
            "orbit.task.artifact.get",
            "orbit.task.artifact.put",
            "orbit.task.eligible",
            "orbit.task.list",
            "orbit.task.review_reset",
            "orbit.task.show",
            "orbit.task.update",
            "orbit.workflow.auto",
            "orbit.workflow.run.list",
            "orbit.workflow.run.resume",
            "orbit.workflow.run.show",
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

#[test]
fn register_plugin_tool_refuses_to_overwrite_a_builtin_name() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let builtin = registry
        .get_schema("orbit.command.exec")
        .expect("the built-in is registered");
    assert!(builtin.builtin);

    let binding = Arc::new(PluginToolBinding {
        provenance: PluginProvenance {
            name: "command".to_string(),
            version: "1.0.0".to_string(),
            manifest_digest: "0".repeat(64),
            grants: Vec::new(),
        },
        execution_kind: PluginExecutionKind::ReadOnly,
        diagnostic: None,
    });
    registry.register_plugin_tool(
        PluginImpersonator("orbit.command.exec"),
        Some(McpToolScope::WorkspaceRequired),
        binding,
    );

    let kept = registry
        .get_schema("orbit.command.exec")
        .expect("the name is still registered");
    assert!(
        kept.builtin
            && kept.description == builtin.description
            && registry.plugin_binding("orbit.command.exec").is_none(),
        "the built-in still holds orbit.command.exec"
    );
}

struct ReadOnlyPlugin;

impl Tool for ReadOnlyPlugin {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "demo.inspect".to_string(),
            description: "read-only plugin tool".to_string(),
            parameters: Vec::new(),
            builtin: false,
        }
    }

    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn execute(&self, _ctx: &ToolContext, _input: Value) -> Result<Value, OrbitError> {
        Ok(Value::Null)
    }
}

#[test]
fn read_only_tools_advertise_the_hint_and_mutating_ones_do_not() {
    let definitions =
        canonical_builtin_mcp_tool_definitions().expect("builtin MCP definitions are valid");
    let annotations_of = |name: &str| {
        definitions
            .iter()
            .find(|definition| definition.schema.name == name)
            .unwrap_or_else(|| panic!("{name} is advertised"))
            .annotations
            .unwrap_or_else(|| panic!("{name} advertises annotations"))
    };

    for name in [
        "orbit.task.eligible",
        "orbit.task.list",
        "orbit.task.show",
        "orbit.task.artifact.get",
        "orbit.search",
        "orbit.auto_task.list",
        "orbit.workflow.run.list",
        "orbit.workflow.run.show",
    ] {
        assert_eq!(annotations_of(name).read_only, Some(true), "{name}");
    }
    for name in [
        "orbit.task.add",
        "orbit.task.update",
        "orbit.task.artifact.put",
        "orbit.friction.add",
        "orbit.friction.update",
        "orbit.workflow.ship",
        "orbit.agent.invoke",
        "orbit.command.exec",
    ] {
        assert_eq!(annotations_of(name).read_only, Some(false), "{name}");
    }

    // `rehome_to` moves the record and resolves the original.
    assert_eq!(
        annotations_of("orbit.friction.update").destructive,
        Some(true)
    );
    assert_eq!(annotations_of("orbit.task.add").destructive, Some(false));
    assert_eq!(annotations_of("orbit.command.exec").open_world, Some(true));
    assert_eq!(annotations_of("orbit.task.list").open_world, Some(false));
}

#[test]
fn advertised_read_only_hint_agrees_with_every_tools_execution_kind() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let definitions = registry
        .mcp_tool_definitions()
        .expect("builtin MCP definitions are valid");
    for definition in definitions {
        let name = definition.schema.name.as_str();
        let kind = registry
            .execution_kind(name)
            .unwrap_or_else(|| panic!("{name} is registered"));
        let annotations = definition
            .annotations
            .unwrap_or_else(|| panic!("{name} advertises annotations"));
        assert_eq!(
            annotations.read_only,
            Some(kind == ToolExecutionKind::ReadOnly),
            "{name}: clients auto-approve on readOnlyHint, so it must be true exactly when the \
             tool's execution kind is ReadOnly"
        );
        if annotations.read_only == Some(true) {
            assert_eq!(annotations.destructive, None, "{name}");
        }
    }
}

#[test]
fn plugin_tools_advertise_only_the_read_only_fact_their_kind_proves() {
    let mut registry = ToolRegistry::new();
    let binding = Arc::new(PluginToolBinding {
        provenance: PluginProvenance {
            name: "demo".to_string(),
            version: "1.0.0".to_string(),
            manifest_digest: "0".repeat(64),
            grants: Vec::new(),
        },
        execution_kind: PluginExecutionKind::ReadOnly,
        diagnostic: None,
    });
    registry.register_plugin_tool(
        ReadOnlyPlugin,
        Some(McpToolScope::WorkspaceRequired),
        binding,
    );
    let definitions = registry.mcp_tool_definitions().expect("valid");
    let annotations = definitions[0].annotations.expect("annotations");
    assert_eq!(annotations.read_only, Some(true));
    assert_eq!(annotations.destructive, None);
    assert_eq!(annotations.open_world, None);
}
