#![allow(missing_docs)]
// Tests use unwrap/expect to keep fixture setup readable.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;

use orbit_common::protocol::tool_input::RETIRED_TASK_ADD_INPUT_FIELDS;
use orbit_tools::{ToolContext, ToolRegistry};

const RETIRED_AGENT_TOOL_NAMES: &[&str] = &[
    "git.push",
    "github.pr.comment",
    "github.pr.comment.reply",
    "github.pr.comments",
    "github.pr.create",
    "github.pr.merge",
    "github.pr.review",
    "github.pr.review.comment",
    "github.pr.view",
    "orbit.state.get",
    "orbit.state.set",
];

const INACTIVE_TOOL_NAMES: &[&str] = &[
    // ORB-10798: auto-task definitions are authored by humans; the agent
    // surface keeps only `list` and `mint`.
    "orbit.auto_task.add",
    "orbit.auto_task.show",
    "orbit.auto_task.toggle",
    "orbit.auto_task.update",
    "orbit.docs.index",
    "orbit.docs.migrate",
    "orbit.docs.add",
    "orbit.docs.list",
    "orbit.docs.show",
    "orbit.task.locks",
    "orbit.task.locks.release",
    "orbit.task.locks.reserve",
    // ORB-10709: workspace claim, a coordination hold like the task locks.
    "orbit.workspace.claim.acquire",
    "orbit.workspace.claim.release",
    "orbit.workspace.claim.show",
    "orbit.semantic.index",
    "orbit.semantic.install",
    "orbit.semantic.stats",
    "orbit.friction.stats",
    // Admin/destructive ops — CLI path retains them, agent MCP surface does
    // not expose them.
    "orbit.semantic.uninstall",
    "orbit.task.delete",
    "orbit.task.lint",
];

#[test]
fn unused_tools_are_not_registered_in_public_surface() {
    let names = registered_tool_names();

    for removed in [
        "git.commit",
        "git.stage_paths",
        "github.pr.checkout",
        "github.pr.checks",
        "github.pr.close",
        "github.repo.view",
        "net.http",
        "orbit.session_log.append",
        "orbit.session_log.list",
        "orbit.session_log.resolve",
        "proc.which",
        "time.now",
        "time.sleep",
    ] {
        assert!(
            !names.contains(removed),
            "removed tool still registered: {removed}"
        );
    }

    // Constructed without dotted literals so a repo-wide grep for the
    // retired fs family stays clean after ORB-10828 / ORB-10833.
    for op in [
        "read", "delete", "copy", "create", "ls", "mkdir", "move", "patch", "write",
    ] {
        let removed = format!("fs.{op}");
        assert!(
            !names.contains(removed.as_str()),
            "removed tool still registered: {removed}"
        );
    }

    for removed in RETIRED_AGENT_TOOL_NAMES {
        assert!(
            !names.contains(*removed),
            "retired agent tool still registered: {removed}"
        );
    }

    assert!(
        names.iter().all(|name| !name.starts_with("orbit.adr.")),
        "retired orbit.adr tool family still registered"
    );

    let removed_prefix = "orbit.semantic.";
    for removed in ["related", "search"] {
        let name = format!("{removed_prefix}{removed}");
        assert!(
            !names.contains(name.as_str()),
            "removed tool still registered: {name}"
        );
    }

    let removed_docs_reindex = ["orbit.docs", "reindex"].join(".");
    assert!(
        !names.contains(removed_docs_reindex.as_str()),
        "removed docs reindex tool still registered"
    );
}

#[test]
fn retired_agent_tools_are_absent_from_every_registry_surface_and_dispatch() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let all_names = registry
        .all_schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect::<BTreeSet<_>>();

    for retired in RETIRED_AGENT_TOOL_NAMES {
        assert!(
            !all_names.contains(*retired),
            "retired agent tool remains inspectable: {retired}"
        );
        let error = registry
            .execute(retired, &ToolContext::default(), serde_json::json!({}))
            .expect_err("retired agent tool dispatch must fail");
        assert!(
            error.to_string().contains(retired),
            "dispatch error must name retired tool {retired}: {error}"
        );
    }
}

/// The read-only GitHub discovery surface a CI-remediation body depends on.
///
/// `github.pr.list` was retired with the write-side PR tools; it is back
/// deliberately, as a read-only listing, because a body that cannot enumerate
/// open pull-request heads cannot tell a current failure from a stale one.
/// Nothing that mutates GitHub joined it.
#[test]
fn read_only_github_ci_discovery_is_registered_and_write_operations_are_not() {
    let names = registered_tool_names();

    for retained in [
        "github.auth.status",
        "github.pr.list",
        "github.run.list",
        "github.run.logs",
        "github.run.view",
    ] {
        assert!(
            names.contains(retained),
            "CI discovery tool missing from the agent surface: {retained}"
        );
    }

    for absent in [
        "github.pr.checkout",
        "github.pr.checks",
        "github.pr.close",
        "github.repo.view",
    ] {
        assert!(
            !names.contains(absent),
            "non-discovery github tool must stay unregistered: {absent}"
        );
    }
}

/// A bounded `github.run.logs` is the whole point of routing log reads through
/// a tool: an unbounded one would hand a multi-megabyte runner log straight to
/// the executing agent.
#[test]
fn run_logs_advertises_its_output_bound() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let schema = registry
        .get_active_schema("github.run.logs")
        .expect("github.run.logs schema");
    assert!(
        schema
            .parameters
            .iter()
            .any(|param| param.name == "max_bytes" && !param.required),
        "github.run.logs must expose an optional output bound"
    );
}

#[test]
fn remote_discovery_is_not_registered_in_the_generic_tool_surface() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let all_names = registry
        .all_schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect::<BTreeSet<_>>();

    let remote_owned = "orbit.workspace.list";
    assert!(
        !all_names.contains(remote_owned),
        "Remote-owned discovery leaked into generic ToolRegistry: {remote_owned}"
    );
}

#[test]
fn workflow_critical_tools_remain_registered() {
    let names = registered_tool_names();

    for retained in [
        "orbit.pipeline.invoke",
        "orbit.pipeline.wait",
        "orbit.search",
        "orbit.workflow.ship",
        "orbit.workflow.run.show",
        "orbit.workflow.run.list",
        "orbit.workflow.run.resume",
        "orbit.workflow.run.workers",
        // ORB-00289: `orbit.semantic.uninstall` is inactive on the agent
        // surface; its inactive-classification is covered by
        // `inactive_ops_tools_*` and `INACTIVE_TOOL_NAMES` above.
        "orbit.task.artifact.get",
        "orbit.task.artifact.put",
        "proc.spawn",
    ] {
        assert!(
            names.contains(retained),
            "workflow-critical tool missing: {retained}"
        );
    }
}

#[test]
fn inactive_ops_tools_are_hidden_from_default_registry_surface() {
    let names = registered_tool_names();

    for inactive in INACTIVE_TOOL_NAMES {
        assert!(
            !names.contains(*inactive),
            "inactive tool must be hidden from default registry schemas: {inactive}"
        );
    }
}

#[test]
fn inactive_ops_tools_remain_auditable_in_full_registry_surface() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let all_names = registry
        .all_schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect::<BTreeSet<_>>();

    for inactive in INACTIVE_TOOL_NAMES {
        assert!(
            all_names.contains(*inactive),
            "inactive tool must remain registered for inspection: {inactive}"
        );
        assert!(
            !registry.is_active(inactive),
            "inactive tool must be marked inactive in the registry: {inactive}"
        );
    }
}

#[test]
fn global_search_schema_drops_retired_semantic_tuning_params() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let schema = registry
        .get_schema("orbit.search")
        .expect("global search schema");
    let names = schema
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();

    assert!(!names.contains(&"field"));
    assert!(!names.contains(&"embedding_model"));
}

#[test]
fn friction_surface_supports_artifact_triage() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let active: BTreeSet<String> = registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    let all: BTreeSet<String> = registry
        .all_schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();

    for retained in [
        "orbit.friction.add",
        "orbit.friction.list",
        "orbit.friction.update",
    ] {
        assert!(
            active.contains(retained),
            "agent-facing friction tool missing from active surface: {retained}"
        );
    }

    for removed in ["orbit.friction.delete", "orbit.friction.reject"] {
        assert!(
            !all.contains(removed),
            "destructive friction tool registered: {removed}"
        );
    }

    // Single-record reads `list` already covers, the tag taxonomy, destructive
    // resolution, and aggregate stats remain CLI / dashboard only [ORB-10798].
    for cli_only in [
        "orbit.friction.show",
        "orbit.friction.tags",
        "orbit.friction.resolve",
        "orbit.friction.stats",
    ] {
        assert!(
            !active.contains(cli_only),
            "{cli_only} must stay hidden from the default registry surface"
        );
        assert!(
            all.contains(cli_only),
            "{cli_only} must remain reachable via `runtime.run_tool`"
        );
    }
}

#[test]
fn auto_task_surface_exposes_only_list_and_mint() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let active: BTreeSet<String> = registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();

    let auto_task: BTreeSet<&str> = active
        .iter()
        .map(String::as_str)
        .filter(|name| name.starts_with("orbit.auto_task."))
        .collect();
    assert_eq!(
        auto_task,
        BTreeSet::from(["orbit.auto_task.list", "orbit.auto_task.mint"])
    );

    let mint = registry
        .get_schema("orbit.auto_task.mint")
        .expect("orbit.auto_task.mint schema");
    assert_eq!(mint.parameters.len(), 1);
    assert_eq!(mint.parameters[0].name, "name");
    assert!(mint.parameters[0].required);
}

#[test]
fn task_add_schema_uses_trimmed_authoring_surface() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let schema = registry
        .get_schema("orbit.task.add")
        .expect("orbit.task.add schema");
    let names = schema
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        names,
        vec![
            "title",
            "description",
            "workspace",
            "acceptance_criteria",
            "tags",
            "required_tools",
            "context_files",
            "allow_missing_context",
            "priority",
            "complexity",
            "type",
            "relations",
            "crew",
            "orchestrator",
            "model",
        ]
    );
    for removed in RETIRED_TASK_ADD_INPUT_FIELDS {
        assert!(
            !names.contains(removed),
            "orbit.task.add schema must not expose retired param {removed}"
        );
    }
}

#[test]
fn task_update_dependency_params_remain_in_agent_tool_schema() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let schema = registry
        .get_schema("orbit.task.update")
        .expect("orbit.task.update schema");
    assert!(
        schema
            .parameters
            .iter()
            .all(|param| param.name != "required_tools"),
        "required_tools is creation-only"
    );
    let dependency_param = schema
        .parameters
        .iter()
        .find(|param| param.name == "dependencies")
        .expect("orbit.task.update dependencies param");

    assert_eq!(dependency_param.param_type, "string_list");
    assert!(!dependency_param.required);
    assert!(
        schema.parameters.iter().any(|param| param.name == "crew"),
        "orbit.task.update should expose crew"
    );
    assert!(
        schema
            .parameters
            .iter()
            .any(|param| param.name == "orchestrator"),
        "orbit.task.update should expose explicit orchestration attribution"
    );
}

#[test]
fn task_show_schema_distinguishes_execution_crew_from_orchestrator() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let schema = registry
        .get_schema("orbit.task.show")
        .expect("orbit.task.show schema");
    let fields = schema
        .parameters
        .iter()
        .find(|param| param.name == "fields")
        .expect("task show fields parameter");
    assert!(fields.description.contains("crew"));
    assert!(fields.description.contains("orchestrator"));
    for field in [
        "status",
        "id",
        "title",
        "type",
        "priority",
        "complexity",
        "created_at",
        "updated_at",
        "relations",
        "job_run_id",
        "external_refs",
    ] {
        assert!(
            fields.description.contains(field),
            "task show fields schema must advertise `{field}`: {}",
            fields.description
        );
    }
    assert!(schema.description.contains("execution"));
    assert!(schema.description.contains("orchestration attribution"));
    assert!(
        schema.description.contains("globally unique"),
        "task show must advertise global id resolution: {}",
        schema.description
    );
    let id = schema
        .parameters
        .iter()
        .find(|param| param.name == "id")
        .expect("task show id parameter");
    assert!(
        id.description.contains("Globally unique"),
        "id help must not send callers through workspace selection: {}",
        id.description
    );
    let workspace = schema
        .parameters
        .iter()
        .find(|param| param.name == "workspace")
        .expect("task show optional workspace filter");
    assert!(!workspace.required);
    assert!(
        workspace
            .description
            .contains("resolved globally by default"),
        "workspace help must stay an explicit filter: {}",
        workspace.description
    );
}

#[test]
fn task_mutation_schemas_use_model_only_identity() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    for tool_name in [
        "orbit.task.add",
        "orbit.task.update",
        "orbit.task.artifact.put",
    ] {
        let schema = registry
            .get_schema(tool_name)
            .unwrap_or_else(|| panic!("{tool_name} schema"));
        assert!(
            schema.parameters.iter().any(|param| param.name == "model"),
            "{tool_name} should expose model attribution"
        );
        assert!(
            schema.parameters.iter().all(|param| param.name != "agent"),
            "{tool_name} should not expose agent attribution"
        );
    }
}

#[test]
fn task_delete_schema_exposes_optional_force_boolean() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    let schema = registry
        .get_schema("orbit.task.delete")
        .expect("task delete schema");
    let force_param = schema
        .parameters
        .iter()
        .find(|param| param.name == "force")
        .expect("force param");

    assert_eq!(force_param.param_type, "boolean");
    assert!(!force_param.required);
}

fn registered_tool_names() -> BTreeSet<String> {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect()
}

#[test]
fn advertised_tool_text_uses_only_placeholder_artifact_ids() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();

    for schema in registry.all_schemas() {
        for text in std::iter::once(schema.description.as_str()).chain(
            schema
                .parameters
                .iter()
                .map(|parameter| parameter.description.as_str()),
        ) {
            for prefix in ["ORB-", "ADR-", "L-"] {
                assert!(
                    !text.match_indices(prefix).any(|(index, _)| {
                        text.as_bytes()
                            .get(index + prefix.len())
                            .is_some_and(u8::is_ascii_digit)
                    }),
                    "tool {} advertises a concrete workspace-local artifact ID: {text}",
                    schema.name
                );
            }
            assert!(
                !text.as_bytes().windows(12).any(|window| {
                    window[0] == b'F'
                        && window[1..5].iter().all(u8::is_ascii_digit)
                        && window[5] == b'-'
                        && window[6..8].iter().all(u8::is_ascii_digit)
                        && window[8] == b'-'
                        && window[9..12].iter().all(u8::is_ascii_digit)
                }),
                "tool {} advertises a concrete workspace-local friction ID: {text}",
                schema.name
            );
        }
    }
}
