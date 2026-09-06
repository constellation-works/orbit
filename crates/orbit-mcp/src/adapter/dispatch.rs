use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_types::telemetry::normalize_self_reported_actor;
#[cfg(test)]
use orbit_types::tool::ToolSchema;
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Implementation, InitializeRequestParams,
    InitializeResult, ListToolsResult, Meta, PaginatedRequestParams, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{Map, Value};

use super::OrbitToolServer;
use super::name_map::{ToolNameCollision, build_name_map};
use super::schema::{
    SelectorAdvertisement, WorkspaceBinding, ensure_workspace_selector, schema_to_tool,
};
use super::structured::mcp_tool_call_result;
use crate::error::tool_error_result;

impl OrbitToolServer {
    /// Return the host's complete exposed surface after validating only the
    /// canonical and advertised names the MCP kernel itself owns.
    pub(super) fn tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        let definitions = self.host.list_mcp_tool_definitions()?;
        let schemas = definitions
            .iter()
            .map(|definition| definition.schema.clone())
            .collect::<Vec<_>>();
        if let Some(schema) = schemas.iter().find(|schema| schema.name.trim().is_empty()) {
            return Err(OrbitError::InvalidInput(format!(
                "canonical MCP tool name must not be empty: {:?}",
                schema.name
            )));
        }
        build_name_map(&schemas).map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        Ok(definitions)
    }

    #[cfg(test)]
    pub(super) fn tool_schemas(&self) -> Result<Vec<ToolSchema>, OrbitError> {
        Ok(self
            .tool_definitions()?
            .into_iter()
            .map(|definition| definition.schema)
            .collect())
    }

    /// Resolve the advertised wire input schema for one canonical definition.
    pub(super) fn input_schema_for(
        &self,
        definition: &McpToolDefinition,
    ) -> Result<Map<String, Value>, OrbitError> {
        let mut schema = super::schema::build_input_schema(
            &definition.schema.name,
            &definition.schema.parameters,
        );
        ensure_workspace_selector(&mut schema, definition, self.selector_advertisement());
        Ok(schema)
    }

    /// Whether this session already carries a workspace selector, and can
    /// therefore route a workspace-scoped call that omits one.
    ///
    /// `tools/list` is answered per session, so the advertised selector
    /// documents the session the caller is actually in rather than a generic
    /// "it depends". Federated mux sessions always advertise the host-qualified
    /// copy-from-list form, even if initialize bound a v1 `ws_*`.
    fn selector_advertisement(&self) -> SelectorAdvertisement {
        if self.host.federated_workspace_selectors() {
            SelectorAdvertisement::Federated
        } else {
            SelectorAdvertisement::Authoritative(self.session_workspace_binding())
        }
    }

    fn session_workspace_binding(&self) -> WorkspaceBinding {
        if self.session_context().workspace.is_some() {
            WorkspaceBinding::Bound
        } else {
            WorkspaceBinding::Unbound
        }
    }

    pub(crate) fn replace_session_context(&self, session_context: ToolSessionContext) {
        if let Ok(mut guard) = self.session_context.write() {
            *guard = session_context;
        }
    }

    /// Fold one client's `initialize` claims into the trusted session envelope.
    ///
    /// Initialize controls only the workspace selector and the caller's claim
    /// about itself. Caller, process, transport, and correlation facts remain
    /// server-owned.
    pub(crate) fn adopt_announced_session(&self, announced: ToolSessionContext) {
        let mut trusted = self.session_context();
        // A client that announces no workspace falls back to the one this
        // server was launched for, rather than clearing the selector: most MCP
        // clients cannot put `_meta.orbit.workspace` on their initialize at
        // all, so a managed integration would otherwise have to repeat the
        // selector on every workspace-scoped call. An announced workspace
        // still wins, and the fallback is the immutable launch value, so a
        // re-initialize never inherits the previous client's claim.
        trusted.workspace = announced
            .workspace
            .or_else(|| self.launch_workspace.clone());
        // ORB-10890: recorded as untrusted evidence beside the trusted role,
        // never merged into it. A re-initialize replaces the claim outright so
        // one session can never accumulate two identities; `None` here means
        // this session is anonymous, not that the previous claim still holds.
        trusted.self_reported_actor = announced.self_reported_actor;
        trusted.trace_id = None;
        self.replace_session_context(trusted);
    }

    pub(crate) fn session_context(&self) -> ToolSessionContext {
        self.session_context
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Clone the trusted session envelope and mint exactly one trace for this
    /// call. The context is not written back, so concurrent calls never share a
    /// trace and initialize-owned session state remains unchanged.
    fn context_for_tool_call(&self) -> ToolSessionContext {
        let mut context = self.session_context();
        context.trace_id = Some(audit_execution_id("trace"));
        context
    }

    pub(super) fn canonical_name(&self, advertised: &str) -> Result<String, McpError> {
        let schemas = self
            .tool_definitions()
            .map(|definitions| {
                definitions
                    .into_iter()
                    .map(|definition| definition.schema)
                    .collect::<Vec<_>>()
            })
            .map_err(invalid_definitions_mcp_error)?;
        let map = build_name_map(&schemas).map_err(ToolNameCollision::into_mcp_error)?;
        Ok(map
            .get(advertised)
            .cloned()
            .unwrap_or_else(|| advertised.to_string()))
    }

    #[cfg(test)]
    pub(super) async fn call_tool_request(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_tool_call(request).await
    }

    async fn dispatch_tool_call(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        let call_context = self.context_for_tool_call();
        let canonical = self.canonical_name(request.name.as_ref())?;
        let input = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(Map::new()));

        let host = Arc::clone(&self.host);
        let execution_name = canonical.clone();
        let result = tokio::task::spawn_blocking(move || {
            host.call_tool(&execution_name, input, call_context)
        })
        .await;

        match result {
            Ok(Ok(value)) => Ok(mcp_tool_call_result(value)),
            Ok(Err(error)) => Ok(tool_error_result(&error)),
            Err(join_error) => {
                let error = OrbitError::Execution(format!(
                    "tool '{canonical}' worker panicked or was cancelled: {join_error}"
                ));
                Ok(tool_error_result(&error))
            }
        }
    }
}

impl ServerHandler for OrbitToolServer {
    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        self.adopt_announced_session(session_context_from_initialize(&request, &context.meta));
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request);
        }
        std::future::ready(Ok(self.get_info()))
    }

    fn get_info(&self) -> ServerInfo {
        let implementation = Implementation::new("orbit-mcp", env!("CARGO_PKG_VERSION"));
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        InitializeResult::new(capabilities)
            .with_server_info(implementation)
            .with_instructions(
                "Orbit tool registry exposed over MCP. Call tools/list to discover available \
                 operations; each tool advertises its own input schema.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut definitions = self
            .tool_definitions()
            .map_err(invalid_definitions_mcp_error)?;
        definitions.sort_by(|left, right| left.schema.name.cmp(&right.schema.name));
        let tools = definitions
            .into_iter()
            .map(|definition| {
                let input_schema = self
                    .input_schema_for(&definition)
                    .map_err(invalid_definitions_mcp_error)?;
                Ok(schema_to_tool(definition.schema, input_schema))
            })
            .collect::<Result<Vec<_>, McpError>>()?;
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch_tool_call(request).await
    }
}

fn invalid_definitions_mcp_error(error: OrbitError) -> McpError {
    McpError::internal_error(
        format!("invalid canonical MCP tool definitions: {error}"),
        Some(serde_json::json!({ "code": "invalid_tool_definitions" })),
    )
}

pub(super) fn session_context_from_initialize(
    request: &InitializeRequestParams,
    transport_meta: &Meta,
) -> ToolSessionContext {
    // rmcp moves wire `_meta` to the request context. Prefer params-level meta
    // for in-process callers, then fall back to the transport-level value.
    let workspace = meta_string(request.meta.as_ref().map(|meta| &meta.0), "workspace")
        .or_else(|| meta_string(Some(&transport_meta.0), "workspace"));

    ToolSessionContext {
        workspace,
        self_reported_actor: self_reported_actor_from_initialize(request, transport_meta),
        ..ToolSessionContext::default()
    }
}

/// Resolve the identity the client claims for itself, in the one place the MCP
/// protocol gives a client to describe itself: `initialize` [ORB-10890].
///
/// `_meta.orbit.actor` wins over `clientInfo.name` because the two answer
/// different questions. `clientInfo` names the *product* that opened the
/// session (`claude-code`, `codex`), which every MCP client sends and which is
/// the useful default; `_meta.orbit.actor` lets an agent that knows its own
/// family or model say so. Both are equally unverified — the precedence is
/// about specificity, not trust. The client's `version` is deliberately
/// excluded so a per-agent denominator does not fragment on every client
/// release.
///
/// A claim that is absent, blank, or malformed yields `None`, which records as
/// anonymous. There is no fallback to another source of identity.
fn self_reported_actor_from_initialize(
    request: &InitializeRequestParams,
    transport_meta: &Meta,
) -> Option<String> {
    meta_string(request.meta.as_ref().map(|meta| &meta.0), "actor")
        .or_else(|| meta_string(Some(&transport_meta.0), "actor"))
        .or_else(|| Some(request.client_info.name.clone()))
        .as_deref()
        .and_then(normalize_self_reported_actor)
}

/// Read `_meta.orbit.<key>`, accepting both the nested object and the flat
/// dotted spelling clients use when they cannot build nested `_meta`.
fn meta_string(meta: Option<&rmcp::model::JsonObject>, key: &str) -> Option<String> {
    meta.and_then(|meta| {
        meta.get("orbit")
            .and_then(|orbit| orbit.get(key))
            .or_else(|| meta.get(&format!("orbit.{key}")))
            .and_then(Value::as_str)
    })
    .map(str::trim)
    .filter(|value| !value.is_empty())
    .map(ToOwned::to_owned)
}
