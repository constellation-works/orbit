use std::sync::Arc;

use orbit_types::tool::{
    McpToolDefinition, McpToolScope, McpTransport, ToolParam, ToolSchema, ToolSessionContext,
};
use rmcp::model::{ClientCapabilities, Implementation, InitializeRequestParams, Meta};

use super::super::dispatch::session_context_from_initialize;
use super::super::schema::{
    build_input_schema, build_input_schema_with_friction_taxonomy, property_for, schema_to_tool,
};
use serde_json::{Value, json};

use super::super::OrbitToolServer;
use super::super::test_support::{
    SessionContextHost, param, param_with_type, request_with_args, tool_schema,
};

#[test]
fn task_schema_uses_common_domain_enums() {
    let schema = build_input_schema("orbit.task.add", &[param("type"), param("status")]);
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");

    assert!(properties["type"]["enum"].as_array().is_some());
    assert!(properties["status"]["enum"].as_array().is_some());
}

#[test]
fn task_update_schema_uses_common_status_enum() {
    let schema = build_input_schema("orbit.task.update", &[param("status")]);
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");
    assert!(properties["status"]["enum"].as_array().is_some());
}

#[test]
fn friction_add_schema_advertises_live_taxonomy_title_limit_and_required_body() {
    let params = vec![
        ToolParam {
            name: "body".to_string(),
            description: "Markdown body".to_string(),
            param_type: "string".to_string(),
            required: true,
        },
        param_with_type("title", "string"),
        param_with_type("tags", "string_list"),
    ];
    let taxonomy = vec![
        ("build".to_string(), "Build and CI failures".to_string()),
        (
            "workspace-special".to_string(),
            "Local operator category".to_string(),
        ),
    ];
    let schema =
        build_input_schema_with_friction_taxonomy("orbit.friction.add", &params, Some(&taxonomy));
    let properties = schema["properties"].as_object().expect("properties");

    assert_eq!(properties["title"]["maxLength"], json!(120));
    assert!(
        schema["required"]
            .as_array()
            .is_some_and(|required| { required.iter().any(|field| field == "body") })
    );
    let description = properties["tags"]["description"]
        .as_str()
        .expect("tag description");
    assert!(description.contains("`build` — Build and CI failures"));
    assert!(description.contains("`workspace-special` — Local operator category"));
    assert!(!description.contains("may extend this default vocabulary"));
    let item_enum = properties["tags"]["anyOf"]
        .as_array()
        .and_then(|alternatives| {
            alternatives.iter().find_map(|alternative| {
                alternative
                    .get("items")
                    .and_then(|items| items.get("enum"))
                    .and_then(Value::as_array)
            })
        })
        .expect("array items enum");
    assert_eq!(item_enum, &vec![json!("build"), json!("workspace-special")]);
}

#[test]
fn unbound_friction_schema_uses_described_defaults_and_says_workspace_may_extend_them() {
    let schema = build_input_schema(
        "orbit.friction.update",
        &[param_with_type("tags", "string_list")],
    );
    let description = schema["properties"]["tags"]["description"]
        .as_str()
        .expect("tag description");

    assert!(description.contains("`build` — make/fmt/lint friction"));
    assert!(description.contains("may extend this default vocabulary"));
    assert!(description.contains("ci → build"));
}

struct LiveTaxonomyHost;

impl crate::McpHost for LiveTaxonomyHost {
    fn list_mcp_tool_definitions(
        &self,
    ) -> Result<Vec<McpToolDefinition>, orbit_common::OrbitError> {
        Ok(Vec::new())
    }

    fn friction_tag_taxonomy(
        &self,
        session_context: &ToolSessionContext,
    ) -> Result<Option<Vec<(String, String)>>, orbit_common::OrbitError> {
        assert_eq!(session_context.workspace.as_deref(), Some("ws_schema"));
        Ok(Some(vec![(
            "workspace-special".to_string(),
            "Workspace description".to_string(),
        )]))
    }

    fn call_tool(
        &self,
        _name: &str,
        _input: Value,
        _session_context: ToolSessionContext,
    ) -> Result<Value, orbit_common::OrbitError> {
        Ok(Value::Null)
    }
}

#[test]
fn bound_server_schema_reads_taxonomy_from_its_workspace_host() {
    let definition = definition_with_scope(
        "orbit.friction.add",
        vec![param_with_type("tags", "string_list")],
        McpToolScope::WorkspaceRequired,
    );
    let server = OrbitToolServer::new_with_context(
        Arc::new(LiveTaxonomyHost),
        ToolSessionContext::with_workspace("ws_schema"),
    );
    let schema = server
        .input_schema_for(&definition)
        .expect("bound schema resolves");
    let description = schema["properties"]["tags"]["description"]
        .as_str()
        .expect("tag description");

    assert!(description.contains("`workspace-special` — Workspace description"));
    assert!(!description.contains("may extend this default vocabulary"));
}

#[test]
fn schema_to_tool_keeps_dotted_orbit_tools_advertised_with_underscores() {
    let schema = tool_schema("orbit.task.add");
    let input_schema = build_input_schema(&schema.name, &schema.parameters);
    let tool = schema_to_tool(schema, input_schema);
    assert_eq!(tool.name.as_ref(), "orbit_task_add");
}

#[test]
fn task_dependency_schemas_accept_string_or_string_array() {
    let schema = build_input_schema(
        "orbit.task.update",
        &[param_with_type("dependencies", "string_list")],
    );
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");
    let dependencies = properties
        .get("dependencies")
        .and_then(Value::as_object)
        .expect("dependencies property");
    let any_of = dependencies
        .get("anyOf")
        .and_then(Value::as_array)
        .expect("string-list union");

    assert!(
        any_of.iter().any(|schema| {
            schema.get("type").and_then(Value::as_str) == Some("array")
                && schema
                    .get("items")
                    .and_then(|items| items.get("type"))
                    .and_then(Value::as_str)
                    == Some("string")
        }),
        "orbit.task.update dependencies must accept an array of strings"
    );
    assert!(
        any_of
            .iter()
            .any(|schema| schema.get("type").and_then(Value::as_str) == Some("string")),
        "orbit.task.update dependencies must accept a string"
    );
}

/// A handler-required list parameter must be advertised in `required` as a
/// string|array union. Otherwise a schema-following caller can omit it and the
/// backend rejects the call only after dispatch.
#[test]
fn required_string_list_param_is_advertised_as_string_or_array() {
    let selectors = ToolParam {
        name: "selectors".to_string(),
        description: "Selector string or array.".to_string(),
        param_type: "string_list".to_string(),
        required: true,
    };
    let summary = param_with_type("summary", "boolean");
    let schema = build_input_schema("fixture.batch", &[selectors, summary]);

    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("required array present");
    assert!(
        required.iter().any(|value| value == "selectors"),
        "selectors must be advertised as required: {required:?}"
    );
    assert!(
        !required.iter().any(|value| value == "summary"),
        "optional params must not appear in required: {required:?}"
    );

    // Advertised as a string|array union so a bare string or an array validate.
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties");
    let any_of = properties["selectors"]["anyOf"]
        .as_array()
        .expect("selectors string-list union");
    assert!(
        any_of
            .iter()
            .any(|shape| shape.get("type").and_then(Value::as_str) == Some("array")),
        "selectors must accept an array: {any_of:?}"
    );
    assert!(
        any_of
            .iter()
            .any(|shape| shape.get("type").and_then(Value::as_str) == Some("string")),
        "selectors must accept a bare string: {any_of:?}"
    );
}

// The authoritative server's workspace selector must be
// advertised on every workspace-scoped tool. A managed executor speaks through
// a general-purpose MCP client that cannot inject initialize `_meta`, so the
// call argument is its only routing surface.

fn definition_with_scope(
    name: &str,
    parameters: Vec<ToolParam>,
    scope: McpToolScope,
) -> McpToolDefinition {
    let schema = ToolSchema {
        name: name.to_string(),
        description: String::new(),
        parameters,
        builtin: true,
    };
    McpToolDefinition::new(schema, scope)
}

fn advertised_properties(definition: &McpToolDefinition) -> serde_json::Map<String, Value> {
    advertised_properties_on(definition, SessionContextHost::default())
}

fn advertised_properties_on(
    definition: &McpToolDefinition,
    host: SessionContextHost,
) -> serde_json::Map<String, Value> {
    let server = OrbitToolServer::new(Arc::new(host));
    let schema = server
        .input_schema_for(definition)
        .expect("input schema resolves");
    schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .expect("properties object")
}

#[test]
fn workspace_scoped_tool_advertises_the_authoritative_server_selector() {
    let definition = definition_with_scope(
        "orbit.task.list",
        vec![param("limit")],
        McpToolScope::WorkspaceRequired,
    );

    let properties = advertised_properties(&definition);
    let workspace = properties
        .get("workspace")
        .and_then(Value::as_object)
        .expect("workspace selector advertised on a workspace-scoped tool");

    assert_eq!(
        workspace.get("type").and_then(Value::as_str),
        Some("string")
    );
    let description = workspace
        .get("description")
        .and_then(Value::as_str)
        .expect("selector carries routing guidance");
    assert!(
        description.contains("registered workspace name"),
        "selector must document the registered-name form: {description}"
    );
    assert!(
        description.contains("ws_*"),
        "selector must document logical ws_* ids: {description}"
    );
    assert!(
        description.contains("absolute path registered on that server"),
        "selector must document the server-side path form: {description}"
    );
}

#[test]
fn task_show_skips_generic_workspace_selector_injection() {
    let definition = definition_with_scope(
        "orbit.task.show",
        vec![param("id")],
        McpToolScope::WorkspaceRequired,
    );

    let properties = advertised_properties(&definition);
    assert!(
        !properties.contains_key("workspace"),
        "generic workspace-selection machinery must not inject a selector on task.show: {properties:?}"
    );
}

#[test]
fn task_show_own_workspace_param_documents_global_id_resolution() {
    let declared = ToolParam {
        name: "workspace".to_string(),
        description: "Optional explicit workspace filter. `id` is resolved globally by default"
            .to_string(),
        param_type: "string".to_string(),
        required: false,
    };
    let definition = definition_with_scope(
        "orbit.task.show",
        vec![param("id"), declared],
        McpToolScope::WorkspaceRequired,
    );

    let properties = advertised_properties(&definition);
    let workspace = properties
        .get("workspace")
        .and_then(Value::as_object)
        .expect("task.show declares its own optional workspace filter");
    let description = workspace
        .get("description")
        .and_then(Value::as_str)
        .expect("filter carries routing guidance");
    assert!(
        description.contains("resolved globally by default"),
        "task.show must document global id resolution: {description}"
    );
    assert!(
        !description.contains("_meta.orbit.workspace"),
        "generic session-default copy must not replace the task.show filter docs: {description}"
    );
}

#[test]
fn global_scoped_tool_does_not_advertise_a_workspace_selector() {
    let definition = definition_with_scope(
        "orbit.workspace.list",
        vec![param("limit")],
        McpToolScope::Global,
    );

    let properties = advertised_properties(&definition);

    assert!(
        !properties.contains_key("workspace"),
        "a global tool routes without a workspace: {properties:?}"
    );
}

#[test]
fn tool_declaring_its_own_workspace_param_keeps_that_description() {
    let declared = ToolParam {
        name: "workspace".to_string(),
        description: "Workspace path for the task".to_string(),
        param_type: "string".to_string(),
        required: false,
    };
    let definition = definition_with_scope(
        "orbit.task.add",
        vec![param("title"), declared],
        McpToolScope::WorkspaceRequired,
    );

    let properties = advertised_properties(&definition);

    assert_eq!(
        properties["workspace"]
            .get("description")
            .and_then(Value::as_str),
        Some("Workspace path for the task"),
        "injection must not overwrite a tool's own selector documentation"
    );
}

/// A server launched for a workspace advertises the selector as optional, and
/// says why, so an agent in that session does not have to discover a selector
/// it will never need.
#[test]
fn a_launch_bound_session_advertises_the_selector_as_optional() {
    let definition = definition_with_scope(
        "orbit.task.update",
        vec![param("id")],
        McpToolScope::WorkspaceRequired,
    );

    let description = selector_description(&definition, Some("ws_orbit"));

    assert!(
        description.contains("Optional in this session"),
        "a bound session must advertise the selector as optional: {description}"
    );
    assert!(
        description.contains("orbit mcp serve --workspace"),
        "the binding's origin must be named: {description}"
    );
}

/// An unbound session must not read as if it could omit the selector: it
/// cannot, and a call that omits one is refused.
#[test]
fn an_unbound_session_advertises_the_selector_as_required() {
    let definition = definition_with_scope(
        "orbit.task.update",
        vec![param("id")],
        McpToolScope::WorkspaceRequired,
    );

    let description = selector_description(&definition, None);

    assert!(
        description.contains("Required in this session"),
        "an unbound session must advertise the selector as required: {description}"
    );
    assert!(
        description.contains("refused"),
        "the fail-closed consequence must be stated: {description}"
    );
    let server = OrbitToolServer::new(Arc::new(SessionContextHost::default()));
    let schema = server.input_schema_for(&definition).expect("input schema");
    assert_eq!(schema["required"], json!(["workspace"]));
}

#[test]
fn bound_session_keeps_declared_requirements_without_requiring_workspace() {
    let definition = definition_with_scope(
        "orbit.task.update",
        vec![ToolParam {
            required: true,
            ..param("id")
        }],
        McpToolScope::WorkspaceRequired,
    );
    let server = OrbitToolServer::new_with_context(
        Arc::new(SessionContextHost::default()),
        ToolSessionContext::with_workspace("ws_orbit"),
    );
    let schema = server.input_schema_for(&definition).expect("input schema");
    assert_eq!(schema["required"], json!(["id"]));
}

#[test]
fn search_schema_requires_a_query_or_tag_and_an_unbound_workspace() {
    let definition = definition_with_scope(
        "orbit.search",
        vec![param("query"), param_with_type("tag", "string_list")],
        McpToolScope::WorkspaceRequired,
    );
    let server = OrbitToolServer::new(Arc::new(SessionContextHost::default()));
    let schema = server.input_schema_for(&definition).expect("input schema");
    assert_eq!(schema["required"], json!(["workspace"]));
    assert_eq!(
        schema["allOf"],
        json!([{ "anyOf": [
        { "required": ["query"] },
        { "required": ["tag"] }
    ] }])
    );
}

fn selector_description(definition: &McpToolDefinition, launch_workspace: Option<&str>) -> String {
    selector_description_on(definition, SessionContextHost::default(), launch_workspace)
}

fn selector_description_on(
    definition: &McpToolDefinition,
    host: SessionContextHost,
    launch_workspace: Option<&str>,
) -> String {
    let context = ToolSessionContext {
        workspace: launch_workspace.map(ToOwned::to_owned),
        ..ToolSessionContext::default()
    };
    let server = OrbitToolServer::new_with_context(Arc::new(host), context);
    server
        .input_schema_for(definition)
        .expect("input schema resolves")["properties"]["workspace"]["description"]
        .as_str()
        .expect("selector carries routing guidance")
        .to_string()
}

/// Federated `tools/list` must tell callers to copy the list token and must
/// not present a v1 local form (cwd, registered name, bare `ws_*`) as valid.
#[test]
fn federated_workspace_scoped_tool_advertises_copy_from_list() {
    let definition = definition_with_scope(
        "orbit.task.list",
        vec![param("limit")],
        McpToolScope::WorkspaceRequired,
    );

    let properties = advertised_properties_on(&definition, SessionContextHost::federated());
    let description = properties["workspace"]
        .get("description")
        .and_then(Value::as_str)
        .expect("federated selector carries routing guidance");

    assert_federated_selector_description(description);
}

#[test]
fn federated_task_show_requires_the_machine_qualified_selector() {
    let declared = ToolParam {
        name: "workspace".to_string(),
        description: "Optional explicit workspace filter. `id` is resolved globally by default"
            .to_string(),
        param_type: "string".to_string(),
        required: false,
    };
    let definition = definition_with_scope(
        "orbit.task.show",
        vec![param("id"), declared],
        McpToolScope::WorkspaceRequired,
    );

    let properties = advertised_properties_on(&definition, SessionContextHost::federated());
    let description = properties["workspace"]
        .get("description")
        .and_then(Value::as_str)
        .expect("federated task.show advertises the host-qualified selector");

    assert_federated_selector_description(description);
    let server = OrbitToolServer::new(Arc::new(SessionContextHost::federated()));
    let schema = server.input_schema_for(&definition).expect("input schema");
    assert_eq!(schema["required"], json!(["workspace"]));
}

/// A v1 `ws_*` initialize binding must not rewrite federated list copy as
/// "optional in this session" with registered-name / bare-`ws_*` wording.
#[test]
fn federated_advertisement_ignores_a_v1_session_binding() {
    let definition = definition_with_scope(
        "orbit.task.update",
        vec![param("id")],
        McpToolScope::WorkspaceRequired,
    );

    let description = selector_description_on(
        &definition,
        SessionContextHost::federated(),
        Some("ws_orbit"),
    );

    assert_federated_selector_description(&description);
    assert!(
        !description.contains("Optional in this session"),
        "federated list must not inherit v1 bound-session wording: {description}"
    );
}

fn assert_federated_selector_description(description: &str) {
    assert!(
        description.contains("selector") && description.contains("orbit.workspace.list"),
        "federated selector must instruct copying from the federated list: {description}"
    );
    let lowered = description.to_ascii_lowercase();
    assert!(
        lowered.contains("copy"),
        "federated selector must say to copy the list field: {description}"
    );
    assert!(
        !description.contains("registered workspace name"),
        "federated selector must not present a registered name as valid: {description}"
    );
    assert!(
        !description.contains("ws_*"),
        "federated selector must not present a bare ws_* as valid: {description}"
    );
    assert!(
        !lowered.contains("cwd") && !description.contains("absolute path"),
        "federated selector must not present cwd or an absolute path as valid: {description}"
    );
    assert!(
        !description.contains("orbit mcp serve --workspace"),
        "federated serve does not take --workspace: {description}"
    );
}

/// The managed-executor shape: the server knows its workspace, the client
/// cannot say anything about one, and the session must still route.
#[test]
fn initialize_without_an_announced_workspace_keeps_the_launch_binding() {
    let server = launch_bound_server(Some("ws_orbit"));

    server.adopt_announced_session(session_context_from_initialize(
        &InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new("claude-code", "0"),
        ),
        &Meta::new(),
    ));

    assert_eq!(
        server.session_context().workspace.as_deref(),
        Some("ws_orbit")
    );
}

#[test]
fn an_announced_workspace_overrides_the_launch_binding() {
    let server = launch_bound_server(Some("ws_orbit"));

    server.adopt_announced_session(session_context_from_initialize(
        &initialize_params_with_meta(json!({ "orbit": { "workspace": "/repo/other" } })),
        &Meta::new(),
    ));

    assert_eq!(
        server.session_context().workspace.as_deref(),
        Some("/repo/other")
    );
}

/// The fallback is the immutable launch value, never the previous client's
/// claim, so one session cannot inherit a workspace it was never bound to.
#[test]
fn re_initialize_falls_back_to_the_launch_binding_not_the_previous_claim() {
    let server = launch_bound_server(Some("ws_orbit"));
    server.adopt_announced_session(session_context_from_initialize(
        &initialize_params_with_meta(json!({ "orbit": { "workspace": "/repo/other" } })),
        &Meta::new(),
    ));

    server.adopt_announced_session(session_context_from_initialize(
        &InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new("claude-code", "0"),
        ),
        &Meta::new(),
    ));

    assert_eq!(
        server.session_context().workspace.as_deref(),
        Some("ws_orbit")
    );
}

/// A standalone server binds nothing, so a silent client stays unbound and
/// every workspace-scoped call must name its own workspace.
#[test]
fn an_unbound_server_stays_unbound_when_the_client_announces_nothing() {
    let server = launch_bound_server(None);

    server.adopt_announced_session(session_context_from_initialize(
        &InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new("claude-code", "0"),
        ),
        &Meta::new(),
    ));

    assert_eq!(server.session_context().workspace, None);
}

/// A blank binding names nothing, so it must not present as bound.
#[test]
fn a_blank_launch_binding_is_no_binding() {
    let server = launch_bound_server(Some("   "));

    assert_eq!(server.session_context().workspace, None);
}

fn launch_bound_server(launch_workspace: Option<&str>) -> OrbitToolServer {
    OrbitToolServer::new_with_context(
        Arc::new(SessionContextHost::default()),
        ToolSessionContext {
            workspace: launch_workspace.map(ToOwned::to_owned),
            ..ToolSessionContext::default()
        },
    )
}

fn initialize_params_with_meta(meta: Value) -> InitializeRequestParams {
    let mut params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("orbit-test-client", "0"),
    );
    let Value::Object(object) = meta else {
        panic!("test meta must be an object");
    };
    params.meta = Some(Meta(object));
    params
}

#[test]
fn initialize_meta_extracts_orbit_workspace_session_context() {
    let params = initialize_params_with_meta(json!({
        "orbit": {
            "workspace": " /repo/main ",
            "workspace_id": "spoofed-workspace",
            "caller_machine_id": "spoofed-caller",
            "process_machine_id": "spoofed-process",
            "transport": "ssh-mcp",
            "origin_session_id": "spoofed-session",
            "mcp_call_id": "spoofed-call",
            "trace_id": "spoofed-trace",
            "leased_run": {"run_id": "spoofed-run", "lease_id": "spoofed-lease"},
            "role": "admin",
            "model": "spoofed-model",
            "task_id": "spoofed-task"
        },
        "orbit.workspace_id": "also-spoofed"
    }));

    let session_context = session_context_from_initialize(&params, &Meta::new());

    assert_eq!(session_context.workspace.as_deref(), Some("/repo/main"));
    assert_eq!(session_context.workspace_id, None);
    assert_eq!(session_context.caller_machine_id, None);
    assert_eq!(session_context.process_machine_id, None);
    assert_eq!(session_context.transport, None);
    assert_eq!(session_context.origin_session_id, None);
    assert_eq!(session_context.mcp_call_id, None);
    assert_eq!(session_context.trace_id, None);
}

#[test]
fn initialize_transport_meta_extracts_orbit_workspace_session_context() {
    // Over a real transport rmcp strips `_meta` from the params and delivers
    // it through the request context instead; the params-level field stays
    // `None`. The announced session workspace must still be honored.
    let params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("orbit-test-client", "0"),
    );
    let Value::Object(meta_object) = json!({
        "orbit": {
            "workspace": " /repo/main "
        }
    }) else {
        panic!("test meta must be an object");
    };

    let session_context = session_context_from_initialize(&params, &Meta(meta_object));

    assert_eq!(session_context.workspace.as_deref(), Some("/repo/main"));
}

#[test]
fn initialize_params_meta_wins_over_transport_meta() {
    let params = initialize_params_with_meta(json!({
        "orbit": { "workspace": "/repo/params" }
    }));
    let Value::Object(meta_object) = json!({
        "orbit": { "workspace": "/repo/transport" }
    }) else {
        panic!("test meta must be an object");
    };

    let session_context = session_context_from_initialize(&params, &Meta(meta_object));

    assert_eq!(session_context.workspace.as_deref(), Some("/repo/params"));
}

#[test]
fn client_info_name_is_the_default_self_reported_actor() {
    // Every MCP client sends `clientInfo`, so the common case records an
    // actor without the client having to know anything Orbit-specific.
    let params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("Claude  Code", "1.2.3"),
    );

    let session_context = session_context_from_initialize(&params, &Meta::new());

    assert_eq!(
        session_context.self_reported_actor.as_deref(),
        Some("claude code"),
        "the client version must not fragment the claim"
    );
}

#[test]
fn orbit_meta_actor_wins_over_client_info() {
    let params = initialize_params_with_meta(json!({
        "orbit": { "actor": "claude-opus-5" }
    }));

    let session_context = session_context_from_initialize(&params, &Meta::new());

    assert_eq!(
        session_context.self_reported_actor.as_deref(),
        Some("claude-opus-5")
    );
}

#[test]
fn transport_meta_actor_is_read_when_params_meta_is_stripped() {
    let params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("orbit-test-client", "0"),
    );
    let Value::Object(meta_object) = json!({ "orbit.actor": "codex" }) else {
        panic!("test meta must be an object");
    };

    let session_context = session_context_from_initialize(&params, &Meta(meta_object));

    assert_eq!(
        session_context.self_reported_actor.as_deref(),
        Some("codex")
    );
}

#[test]
fn malformed_claim_falls_through_to_anonymous_not_to_client_info() {
    // A claim Orbit refuses must not silently downgrade to the neighbouring
    // identity source — that would attribute the call to something the caller
    // did not name.
    let params = initialize_params_with_meta(json!({
        "orbit": { "actor": "claude\nrole: admin" }
    }));

    let session_context = session_context_from_initialize(&params, &Meta::new());

    assert_eq!(session_context.self_reported_actor, None);
}

#[test]
fn a_client_claiming_nothing_usable_is_anonymous() {
    let params = InitializeRequestParams::new(
        ClientCapabilities::default(),
        Implementation::new("   ", "0"),
    );

    let session_context = session_context_from_initialize(&params, &Meta::new());

    assert_eq!(session_context.self_reported_actor, None);
}

#[test]
fn a_new_claim_replaces_the_previous_one_rather_than_merging() {
    let host = Arc::new(SessionContextHost::default());
    let server = OrbitToolServer::new(host);
    server.replace_session_context(ToolSessionContext {
        self_reported_actor: Some("codex".to_string()),
        ..ToolSessionContext::default()
    });

    let announced = session_context_from_initialize(
        &InitializeRequestParams::new(
            ClientCapabilities::default(),
            Implementation::new("claude-code", "0"),
        ),
        &Meta::new(),
    );
    let mut trusted = server.session_context();
    trusted.self_reported_actor = announced.self_reported_actor;
    server.replace_session_context(trusted);

    assert_eq!(
        server.session_context().self_reported_actor.as_deref(),
        Some("claude-code")
    );
}

#[tokio::test]
async fn mcp_session_context_reaches_tool_calls_without_workspace_input() {
    let host = Arc::new(SessionContextHost::default());
    let server = OrbitToolServer::new(host.clone());
    let mut trusted_context = ToolSessionContext::trusted_local(
        Some("ws_orbit".to_string()),
        Some("hm_local".to_string()),
        Some("local-host".to_string()),
    );
    trusted_context.workspace = Some("/repo/main".to_string());
    trusted_context.origin_session_id = Some("mcp-session-shared".to_string());
    trusted_context.mcp_call_id = Some("legacy-call".to_string());
    server.replace_session_context(trusted_context);

    let explicit = server
        .call_tool_request(request_with_args(
            "orbit.task.list",
            json!({ "workspace": "/repo/main" }),
        ))
        .await
        .expect("explicit workspace call succeeds")
        .structured_content
        .expect("explicit structured content");
    let ambient = server
        .call_tool_request(request_with_args("orbit.task.list", json!({})))
        .await
        .expect("ambient workspace call succeeds")
        .structured_content
        .expect("ambient structured content");

    assert_eq!(ambient, explicit);
    let calls = host.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].2.workspace.as_deref(), Some("/repo/main"));
    assert!(calls[1].1.get("workspace").is_none());
    assert_eq!(calls[0].2.workspace_id.as_deref(), Some("ws_orbit"));
    assert_eq!(calls[0].2.transport, Some(McpTransport::Local));
    assert_eq!(
        calls[0].2.origin_session_id.as_deref(),
        Some("mcp-session-shared")
    );
    assert_eq!(calls[1].2.origin_session_id, calls[0].2.origin_session_id);
    assert_eq!(calls[0].2.mcp_call_id.as_deref(), Some("legacy-call"));
    assert_eq!(calls[1].2.mcp_call_id.as_deref(), Some("legacy-call"));
    assert!(calls[0].2.trace_id.is_some());
    assert!(calls[1].2.trace_id.is_some());
    assert_ne!(calls[0].2.trace_id, calls[1].2.trace_id);
}

// --- ORB-00102 tests: object_list schema + loud fallback + e2e via MCP adapter ---

fn capture_warnings<F, T>(f: F) -> (T, String)
where
    F: FnOnce() -> T,
{
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone)]
    struct CaptureMakeWriter(Arc<Mutex<Vec<u8>>>);
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for CaptureMakeWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            CaptureWriter(Arc::clone(&self.0))
        }
    }
    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CaptureMakeWriter(Arc::clone(&buffer)))
        .with_max_level(LevelFilter::WARN)
        .with_target(true)
        .with_ansi(false)
        .without_time()
        .finish();
    let result = tracing::subscriber::with_default(subscriber, f);
    let logs =
        String::from_utf8(buffer.lock().expect("capture buffer lock").clone()).expect("utf8 logs");
    (result, logs)
}

#[test]
fn property_for_object_list_emits_anyof_array_of_objects_or_string() {
    for token in [
        "object_list",
        "object[]",
        "objects",
        "OBJECT_LIST",
        "object[] ",
    ] {
        let prop = property_for(token);
        let any_of = match prop.get("anyOf").and_then(Value::as_array) {
            Some(any_of) => any_of,
            None => panic!("anyOf present for {token}"),
        };
        let has_array_objects = any_of.iter().any(|s| {
            s.get("type").and_then(Value::as_str) == Some("array")
                && s.get("items")
                    .and_then(|i| i.get("type"))
                    .and_then(Value::as_str)
                    == Some("object")
        });
        let has_string = any_of
            .iter()
            .any(|s| s.get("type").and_then(Value::as_str) == Some("string"));
        assert!(has_array_objects, "{token} must accept array-of-objects");
        assert!(has_string, "{token} must accept string fallback");
    }
}

#[test]
fn property_for_unknown_emits_tracing_warn_at_target() {
    let token = "<unknown-token-not-in-match-arms>";
    let (prop, logs) = capture_warnings(|| property_for(token));
    assert_eq!(
        prop.get("type").and_then(Value::as_str),
        Some("string"),
        "fallback still produces string"
    );
    assert!(
        logs.contains("unknown ToolParam type degrading to string"),
        "warning message present: {logs}"
    );
    assert!(
        logs.contains("orbit.common.tool_schema"),
        "target present: {logs}"
    );
    assert!(
        logs.contains(token),
        "offending token named in event: {logs}"
    );
}

/// ORB-00234/ORB-00255: MCP schema for orbit_task_add advertises the trimmed
/// create-task fields with correct enums (verifiable via debug surfaces or this
/// direct build).
#[test]
fn task_add_schema_exposes_trimmed_fields_with_common_domain_enums() {
    // Use representative params that the real add schema includes (the
    // build_input_schema only cares about the ones passed for enum injection).
    let params = vec![
        param_with_type("title", "string"),
        param_with_type("description", "string"),
        param_with_type("workspace", "string"),
        param_with_type("acceptance_criteria", "string_list"),
        param_with_type("tags", "string_list"),
        param_with_type("context_files", "string_list"),
        param_with_type("priority", "string"),
        param_with_type("complexity", "string"),
        param_with_type("type", "string"),
        param_with_type("relations", "array"),
        param_with_type("crew", "string"),
        param_with_type("model", "string"),
    ];
    let schema = build_input_schema("orbit.task.add", &params);
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("properties object");

    let property_names = properties.keys().map(String::as_str).collect::<Vec<_>>();
    assert_eq!(
        property_names,
        vec![
            "acceptance_criteria",
            "complexity",
            "context_files",
            "crew",
            "description",
            "model",
            "priority",
            "relations",
            "tags",
            "title",
            "type",
            "workspace",
        ]
    );

    assert!(properties["complexity"]["enum"].as_array().is_some());
    assert!(properties["model"]["enum"].as_array().is_some());

    for removed in [
        "plan",
        "status",
        "parent_id",
        "source_task_id",
        "external_refs",
        "context",
        "comment",
        "dependencies",
    ] {
        assert!(
            !properties.contains_key(removed),
            "{removed} must not appear in MCP schema properties for orbit.task.add"
        );
    }

    // crew was un-retired (ORB-10123): it is now an authoring param again.
    assert!(
        properties.contains_key("crew"),
        "crew must appear in MCP schema properties for orbit.task.add"
    );
}
