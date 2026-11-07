//! Conformance tests for schema-adjacent builtin MCP definitions.
#![allow(missing_docs)]
#![allow(clippy::expect_used)]

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_tools::plugin::PluginToolBinding;
use orbit_tools::{
    Tool, ToolContext, ToolExecutionKind, ToolRegistry, builtin_mcp_rules,
    canonical_builtin_mcp_tool_definitions,
};
use orbit_types::plugin::{PluginExecutionKind, PluginProvenance};
use orbit_types::tool::{McpIdRoute, McpToolDefinitionError, McpToolScope, ToolParam, ToolSchema};
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
            "orbit.agents.scorecard",
            "orbit.agents.show",
            "orbit.attention",
            "orbit.auto_task.add",
            "orbit.auto_task.list",
            "orbit.auto_task.mint",
            "orbit.auto_task.update",
            "orbit.automation.explain",
            "orbit.codebase.analytics",
            "orbit.codebase.related",
            "orbit.codebase.show",
            "orbit.command.exec",
            "orbit.decision.digest",
            "orbit.decision.list",
            "orbit.decision.raise",
            "orbit.decision.show",
            "orbit.decision.submit",
            "orbit.evolution.add",
            "orbit.evolution.list",
            "orbit.evolution.show",
            "orbit.experiment.abandon",
            "orbit.experiment.assign",
            "orbit.experiment.clone",
            "orbit.experiment.conclude",
            "orbit.experiment.create",
            "orbit.experiment.list",
            "orbit.experiment.pause",
            "orbit.experiment.readout",
            "orbit.experiment.resume",
            "orbit.experiment.reweight",
            "orbit.experiment.show",
            "orbit.experiment.start",
            "orbit.experiment.update",
            "orbit.friction.add",
            "orbit.friction.update",
            "orbit.lease.check",
            "orbit.lease.expand",
            "orbit.lease.list",
            "orbit.lease.notices",
            "orbit.lease.release",
            "orbit.lease.renew",
            "orbit.lease.request",
            "orbit.lease.show",
            "orbit.notice.close",
            "orbit.notice.list",
            "orbit.notice.post",
            "orbit.notice.show",
            "orbit.orchestrator.scorecard",
            "orbit.orchestrator.show",
            "orbit.pipeline.invoke",
            "orbit.resident.checkpoint",
            "orbit.resident.control",
            "orbit.resident.digest",
            "orbit.resident.grant",
            "orbit.resident.list",
            "orbit.resident.metrics",
            "orbit.resident.relay",
            "orbit.resident.show",
            "orbit.routine.control",
            "orbit.search",
            "orbit.task.add",
            "orbit.task.artifact.get",
            "orbit.task.artifact.put",
            "orbit.task.block_triage",
            "orbit.task.eligible",
            "orbit.task.list",
            "orbit.task.reconcile_review",
            "orbit.task.recover_claim",
            "orbit.task.repilot",
            "orbit.task.requeue",
            "orbit.task.retry_handoff",
            "orbit.task.review_reset",
            "orbit.task.revoke_handoff",
            "orbit.task.show",
            "orbit.task.update",
            "orbit.workflow.auto",
            "orbit.workflow.land",
            "orbit.workflow.run.list",
            "orbit.workflow.run.observe",
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

/// A builtin fixture that addresses one id with an optional workspace but
/// declares no id route at its definition site.
struct UnclassifiedIdTool;

impl Tool for UnclassifiedIdTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.lease.unclassified_fixture".to_string(),
            description: "unclassified id-addressing fixture".to_string(),
            parameters: vec![
                ToolParam {
                    name: "id".to_string(),
                    description: "one id".to_string(),
                    param_type: "string".to_string(),
                    required: true,
                },
                ToolParam {
                    name: "workspace".to_string(),
                    description: "optional workspace".to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, _ctx: &ToolContext, _input: Value) -> Result<Value, OrbitError> {
        Ok(Value::Null)
    }
}

#[test]
fn unclassified_id_addressing_builtin_fails_closed_in_this_target() {
    let mut registry = ToolRegistry::new();
    registry.register_mcp(UnclassifiedIdTool, McpToolScope::WorkspaceRequired);
    assert_eq!(
        registry.mcp_tool_definitions(),
        Err(McpToolDefinitionError::UnclassifiedIdRoute(
            "orbit.lease.unclassified_fixture".to_string()
        )),
        "a builtin that takes a required id and an optional workspace but declares no \
         McpIdRoute at its definition site must fail where the tool is added, not on \
         agent-main after landing"
    );
}

#[test]
fn every_builtin_id_route_is_declared_and_preserves_the_former_tables() {
    // The pre-change hand-kept tables, duplicates removed, are the baseline:
    // every one of these tools must keep its classification and, for
    // workspace-local tools, its reason string verbatim.
    let task_routed = [
        "orbit.task.show",
        "orbit.task.update",
        "orbit.task.repilot",
        "orbit.task.reject",
        "orbit.task.delete",
        "orbit.task.artifact.get",
        "orbit.task.artifact.put",
        "orbit.task.review_reset",
        "orbit.task.reconcile_review",
        "orbit.task.requeue",
    ];
    for name in task_routed {
        assert_eq!(
            builtin_mcp_rules(name).id_route,
            McpIdRoute::Task,
            "{name} must stay task-id-routed"
        );
    }
    let friction = "a friction id is workspace-local and carries no host prefix";
    let decision = "a decision request id is workspace-local and carries no host prefix";
    let evolution = "an evolution record id is workspace-local and carries no host prefix";
    let experiment = "an experiment id is workspace-local and carries no host prefix";
    let run = "a run id is host-local: a run executes where it was admitted";
    let notice = "a notice id is workspace-local and carries no host prefix";
    let workspace_local: [(&str, &str); 26] = [
        ("orbit.friction.show", friction),
        ("orbit.friction.update", friction),
        ("orbit.friction.resolve", friction),
        ("orbit.friction.rehome", friction),
        ("orbit.decision.show", decision),
        ("orbit.decision.answer", decision),
        ("orbit.decision.reverse", decision),
        ("orbit.decision.submit", decision),
        ("orbit.evolution.decline", evolution),
        ("orbit.evolution.show", evolution),
        ("orbit.experiment.update", experiment),
        ("orbit.experiment.clone", experiment),
        ("orbit.experiment.start", experiment),
        ("orbit.experiment.pause", experiment),
        ("orbit.experiment.resume", experiment),
        ("orbit.experiment.assign", experiment),
        ("orbit.experiment.reweight", experiment),
        ("orbit.experiment.conclude", experiment),
        ("orbit.experiment.abandon", experiment),
        ("orbit.experiment.delete", experiment),
        ("orbit.experiment.show", experiment),
        ("orbit.experiment.readout", experiment),
        ("orbit.workflow.run.observe", run),
        ("orbit.workflow.run.show", run),
        ("orbit.workflow.run.resume", run),
        ("orbit.notice.show", notice),
    ];
    for (name, reason) in workspace_local {
        assert_eq!(
            builtin_mcp_rules(name).id_route,
            McpIdRoute::WorkspaceLocal(reason.to_string()),
            "{name} must stay workspace-local with its reason verbatim"
        );
    }

    // And no other builtin is classified, while every builtin that
    // addresses one id with an optional workspace is.
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    for schema in registry.all_schemas() {
        let route = builtin_mcp_rules(&schema.name).id_route;
        let classified = task_routed.contains(&schema.name.as_str())
            || workspace_local.iter().any(|(name, _)| *name == schema.name);
        assert_eq!(
            route != McpIdRoute::Unspecified,
            classified,
            "{}: declared route {route:?} does not match the preserved classification",
            schema.name
        );
    }
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
        agent_callable: false,
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
        agent_callable: false,
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
