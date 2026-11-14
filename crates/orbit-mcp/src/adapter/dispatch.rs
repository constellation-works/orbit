use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{
    self, CallerCapabilities, CallerEnvelope, OperationSurface,
};
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_types::telemetry::normalize_self_reported_actor;
use orbit_types::tool::{
    McpInputDecoration, McpPresentationRead, McpToolDefinition, TOOL_ACTION_FIELD, TOOL_FAMILIES,
    ToolSessionContext, tool_family,
};
use rmcp::ErrorData as McpError;
use rmcp::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, CustomRequest, CustomResult,
    Implementation, InitializeRequestParams, InitializeResult, ListResourcesResult,
    ListToolsResult, MetaObject, PaginatedRequestParams, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, RequestMetaObject, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use serde_json::{Map, Value};

use super::name_map::{advertise_tool_names, advertise_tool_names_in_schema, build_name_map};
use super::presentation;
use super::schema::{
    SelectorAdvertisement, WorkspaceBinding, ensure_workspace_selector, host_owns_plugin_selector,
    schema_to_tool, slim_advertised_property_descriptions,
};
use super::structured::mcp_tool_call_result;
use super::{OrbitToolServer, ToolSurface};
use crate::error::tool_error_result;

impl OrbitToolServer {
    /// Load and validate once per immutable or live host surface snapshot.
    fn load_tool_surface(&self) -> Result<Arc<ToolSurface>, OrbitError> {
        let mut cached = self
            .tool_surface
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dynamic = self.host.dynamic_mcp_tool_definitions()?;
        if let Some(surface) = cached.as_ref()
            && dynamic
                .as_ref()
                .is_none_or(|source| Arc::ptr_eq(source, &surface.source))
        {
            return Ok(Arc::clone(surface));
        }
        let source = match dynamic {
            Some(source) => source,
            None => Arc::new(self.host.list_mcp_tool_definitions()?),
        };
        let mut definitions = source.as_ref().clone();
        definitions
            .retain(|definition| crate::internal_drain_name(&definition.schema.name).is_none());
        if definitions.iter().any(|definition| {
            presentation::is_presentation(&super::name_map::sanitize_tool_name(
                &definition.schema.name,
            ))
        }) {
            return Err(OrbitError::InvalidInput(
                "host tool collides with an adapter presentation entrypoint".into(),
            ));
        }
        if let Some(schema) = definitions
            .iter()
            .map(|definition| &definition.schema)
            .find(|schema| schema.name.trim().is_empty())
        {
            return Err(OrbitError::InvalidInput(format!(
                "canonical MCP tool name must not be empty: {:?}",
                schema.name
            )));
        }
        let schemas = definitions
            .iter()
            .map(|definition| definition.schema.clone())
            .collect::<Vec<_>>();
        let name_map = build_name_map(&schemas).map(Arc::new).map_err(|error| {
            invalid_definitions_mcp_error(OrbitError::InvalidInput(error.to_string()))
        });
        let surface = Arc::new(ToolSurface {
            source,
            definitions: Arc::new(definitions),
            name_map,
            lists: Default::default(),
        });
        *cached = Some(Arc::clone(&surface));
        Ok(surface)
    }

    fn load_tool_definitions(&self) -> Result<Arc<Vec<McpToolDefinition>>, OrbitError> {
        self.load_tool_surface()
            .map(|surface| Arc::clone(&surface.definitions))
    }

    /// Resolve the advertised wire input schema for one canonical definition.
    pub(super) fn input_schema_for(
        &self,
        definition: &McpToolDefinition,
    ) -> Result<Map<String, Value>, OrbitError> {
        let taxonomy = (definition.rules.input_decoration == McpInputDecoration::Friction)
            .then(|| self.host.friction_tag_taxonomy(&self.session_context()))
            .transpose()?
            .flatten();
        let mut schema = match definition.input_schema.as_ref().and_then(Value::as_object) {
            Some(declared) => super::schema::declared_input_schema(declared),
            None => super::schema::build_input_schema(
                &definition.schema.name,
                &definition.schema.parameters,
            ),
        };
        super::schema::apply_input_decoration(
            &mut schema,
            definition.rules.input_decoration,
            taxonomy.as_deref(),
        );
        ensure_workspace_selector(&mut schema, definition, self.selector_advertisement());
        slim_advertised_property_descriptions(&mut schema, &definition.schema.name);
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

    /// Drop tools hidden by the host or refused by the session's fixed grants.
    /// Applied after the response cache, which holds the full surface, so a
    /// toggle flipped mid-session shows on the next `tools/list`. This only
    /// changes advertisement; direct calls retain their authorization checks.
    fn without_hidden_tools(
        &self,
        mut result: ListToolsResult,
        definitions: &[McpToolDefinition],
        name_map: &super::CachedNameMap,
    ) -> Result<ListToolsResult, McpError> {
        let context = self.session_context();
        let caller = CallerCapabilities::resolve(&CallerEnvelope::mcp_session(&context));
        let mut hidden = self.host.hidden_tool_names(&context);
        hidden.extend(
            authorization::GOVERNED_OPERATIONS
                .iter()
                .filter(|operation| {
                    operation.surface == OperationSurface::Tool
                        && authorization::authorize(operation, &caller).is_err()
                })
                .map(|operation| operation.id.to_owned()),
        );
        // A family is hidden only when every action it serves is.
        let hidden_families = TOOL_FAMILIES
            .iter()
            .filter(|family| {
                family
                    .actions
                    .iter()
                    .all(|action| hidden.contains(action.operation))
            })
            .map(|family| family.name.to_string())
            .collect::<Vec<_>>();
        hidden.extend(hidden_families);
        if presentation::reader(definitions, McpPresentationRead::Task)
            .is_some_and(|reader| hidden.contains(&reader.schema.name))
        {
            result
                .tools
                .retain(|tool| !presentation::is_presentation(tool.name.as_ref()));
        }
        if hidden.is_empty() {
            return Ok(result);
        }
        let name_map = name_map.clone()?;
        result.tools.retain(|tool| {
            name_map
                .get(tool.name.as_ref())
                .is_none_or(|canonical| !hidden.contains(canonical))
        });
        Ok(result)
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
        // Replaced, like the actor claim: a re-initialize never inherits it.
        trusted.mcp_client_name = announced.mcp_client_name;
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

    async fn dispatch_tool_call(
        &self,
        request: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        if presentation::is_presentation(request.name.as_ref()) {
            if !self.host.presentation_enabled() {
                return Ok(tool_error_result(&OrbitError::InvalidInput(
                    "UI tools are not enabled for this server".into(),
                )));
            }
            let result = self.dispatch_presentation(request).await;
            return Ok(match result {
                Ok(value) => mcp_tool_call_result(value),
                Err(error) => tool_error_result(&error),
            });
        }
        if let Some(canonical) = crate::internal_drain_name(request.name.as_ref()) {
            let host = Arc::clone(&self.host);
            let context = self.context_for_tool_call();
            let input = Value::Object(request.arguments.unwrap_or_default());
            let result = tokio::task::spawn_blocking(move || {
                host.refuse_internal_drain(canonical, input, context)
            })
            .await
            .map_err(|error| McpError::internal_error(error.to_string(), None))?;
            return Ok(match result {
                Ok(value) => mcp_tool_call_result(value),
                Err(error) => tool_error_result(&error),
            });
        }
        let mut call_context = self.context_for_tool_call();
        // One surface load for this call: the name map and the definitions
        // both come from the same snapshot.
        let surface = self
            .load_tool_surface()
            .map_err(invalid_definitions_mcp_error)?;
        let name_map = surface.name_map.clone()?;
        let canonical = name_map
            .get(request.name.as_ref())
            .cloned()
            .unwrap_or_else(|| request.name.to_string());
        let mut input = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(Map::new()));

        let definitions = &surface.definitions;
        let definition = definitions
            .iter()
            .find(|definition| definition.schema.name == canonical);
        if definition.is_some_and(host_owns_plugin_selector)
            && let Value::Object(arguments) = &mut input
            && let Some(selector) = arguments
                .get("workspace")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        {
            // The host still needs the selector to route this call. Plugin
            // inputs see only properties from their own declared schema.
            arguments.remove("workspace");
            call_context.workspace = Some(selector);
        }

        // The host resolves a selector into its checkout (and a federated mux
        // may translate it again). Return the exact accepted client route token,
        // never the destination's local path or a guessed workspace name.
        let domain_extension =
            definition.is_some_and(|definition| definition.rules.workspace_echo.applies(&input));
        let desktop_selector = domain_extension
            .then(|| {
                input
                    .get("workspace")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .flatten();
        if domain_extension
            && desktop_selector
                .as_deref()
                .is_none_or(|selector| selector.trim().is_empty())
        {
            let fields = definition
                .map(|definition| {
                    definition
                        .rules
                        .workspace_echo
                        .matching_input_fields(&input)
                })
                .unwrap_or_default();
            let trigger = if fields.is_empty() {
                String::new()
            } else {
                format!(
                    "input {} requires this selector; ",
                    fields
                        .iter()
                        .map(|field| format!("`{field}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let guidance = if fields.contains(&"view") {
                "`view` reads never use the session's bound workspace. Pass `workspace` with a \
                 selector copied from `orbit_workspace_list`, or omit `view` to use the bound \
                 workspace"
            } else {
                "pass `workspace` with a selector copied from `orbit_workspace_list`"
            };
            return Ok(tool_error_result(&OrbitError::InvalidInput(format!(
                "tool '{canonical}' requires an explicit workspace selector from discovery: \
                 {trigger}{guidance}"
            ))));
        }
        let host = Arc::clone(&self.host);
        let execution_name = canonical.clone();
        let result = tokio::task::spawn_blocking(move || {
            host.call_tool(&execution_name, input, call_context)
        })
        .await;

        match result {
            Ok(Ok(mut value)) => {
                if let (Some(selector), Some(object)) = (desktop_selector, value.as_object_mut()) {
                    object.insert("workspace".into(), Value::String(selector.clone()));
                    for key in ["catalog", "runs"] {
                        if let Some(nested) = object.get_mut(key).and_then(Value::as_object_mut) {
                            nested.insert("workspace".into(), Value::String(selector.clone()));
                        }
                    }
                }
                Ok(mcp_tool_call_result(value))
            }
            Ok(Err(error)) => Ok(tool_error_result(&error)),
            Err(join_error) => {
                let error = OrbitError::Execution(format!(
                    "tool '{canonical}' worker panicked or was cancelled: {join_error}"
                ));
                Ok(tool_error_result(&error))
            }
        }
    }

    async fn dispatch_presentation(
        &self,
        request: CallToolRequestParams,
    ) -> Result<Value, OrbitError> {
        let definitions = self.load_tool_definitions()?;
        let hidden = self.host.hidden_tool_names(&self.session_context());
        let task_reader = presentation::reader(&definitions, McpPresentationRead::Task)
            .filter(|reader| !hidden.contains(&reader.schema.name));
        let Some(task_reader) = task_reader else {
            return Err(OrbitError::InvalidInput(
                "read-only task presentation unavailable on this host".into(),
            ));
        };
        let input = Value::Object(request.arguments.unwrap_or_default());
        let Some(selection) = presentation::selection(&input)? else {
            return Ok(presentation::empty_panel());
        };
        let presentation::Selection {
            workspace,
            id,
            kind,
        } = selection;
        let Some(id) = id else {
            // The UI must rediscover this selector before issuing any data read.
            return Ok(serde_json::json!({"schema_version":1,"workspace":workspace,"task":null}));
        };
        let host = Arc::clone(&self.host);
        let context = self.context_for_tool_call();
        let task_key = id.clone();
        let selector = workspace.clone();
        // The explicit filter reaches the same host operation, audit and policy
        // checks as an ordinary data call. Client metadata never enters context.
        let run_selection = kind == "run";
        let run_reader = presentation::reader(&definitions, McpPresentationRead::Run)
            .filter(|reader| !hidden.contains(&reader.schema.name))
            .filter(|reader| {
                reader
                    .schema
                    .parameters
                    .iter()
                    .any(|parameter| parameter.name == "view")
            });
        let read_name = if run_selection {
            run_reader.ok_or_else(|| {
                OrbitError::InvalidInput("run presentation unavailable on this host".into())
            })?
        } else {
            task_reader
        }
        .schema
        .name
        .clone();
        // A family reader is called with its show action.
        let read_action = tool_family(&read_name)
            .and_then(|family| family.action("show"))
            .map(|action| action.action);
        let task = tokio::task::spawn_blocking(move || {
            if run_selection {
                let mut read = serde_json::json!({"workspace":selector,"view":"bounded","id":task_key,"limit":25});
                if let Some(action) = read_action {
                    read[TOOL_ACTION_FIELD] = Value::String(action.to_string());
                }
                return host.call_tool(&read_name, read, context);
            }
            host.call_tool(
                &read_name,
                serde_json::json!({
                    "workspace": selector, "id": task_key, "fields": presentation::TASK_FIELDS
                }),
                context,
            )
        })
        .await
        .map_err(|error| OrbitError::Execution(format!("task read worker failed: {error}")))??;
        if run_selection {
            if task["run"]["id"]
                .as_str()
                .or_else(|| task["run"]["run_id"].as_str())
                != Some(id.as_str())
            {
                return Err(OrbitError::InvalidInput(
                    "incompatible run read response".into(),
                ));
            }
            let mut result = task;
            result["workspace"] = Value::String(workspace);
            result["kind"] = Value::String("run".into());
            Ok(result)
        } else {
            presentation::panel(workspace, &id, task)
        }
    }
}

impl OrbitToolServer {
    /// Apply one client's `initialize` to this session: the worker binding it
    /// carries, and its announced workspace and self-reported identity.
    ///
    /// Shared by the handshake and by a session resumed across an executable
    /// handover, which replays the original request instead of asking the
    /// client to initialize again.
    pub(crate) fn apply_initialize(
        &self,
        request: &InitializeRequestParams,
        transport_meta: &RequestMetaObject,
    ) -> Result<(), McpError> {
        let metadata: &MetaObject = request
            .meta
            .as_ref()
            .map(|meta| &meta.0)
            .unwrap_or(&transport_meta.0);
        if let Some(value) = metadata
            .get("orbit")
            .and_then(|value| value.get("worker_invocation"))
            .filter(|value| !value.is_null())
        {
            let binding =
                serde_json::from_value::<orbit_types::tool::WorkerInvocation>(value.clone())
                    .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
            binding
                .validate()
                .map_err(|error| McpError::invalid_params(error.to_string(), None))?;
            let mut session = self.session_context();
            if session.transport != Some(orbit_types::tool::McpTransport::SshMcp)
                || session.remote_caller_machine_id().map(str::trim)
                    != Some(binding.execution.machine_id.as_str())
                || session
                    .worker_invocation
                    .as_ref()
                    .is_some_and(|old| old != &binding)
            {
                return Err(McpError::invalid_params(
                    "worker session binding refused",
                    None,
                ));
            }
            session.worker_invocation = Some(binding);
            session
                .effective_capabilities
                .remove(&orbit_types::tool::McpCapability::Operator);
            self.replace_session_context(session);
        }
        self.adopt_announced_session(session_context_from_initialize(request, transport_meta));
        Ok(())
    }

    /// Peer info for a session resumed after an executable handover.
    ///
    /// [`Self::apply_initialize`] keeps the client's metadata, announced
    /// workspace and worker binding. `serve_directly` then treats this value
    /// as the negotiated peer, so the protocol version is the one `initialize`
    /// would have agreed, not the version the client first requested.
    pub(crate) fn resumed_peer_info(
        &self,
        mut request: InitializeRequestParams,
    ) -> Result<InitializeRequestParams, McpError> {
        self.apply_initialize(&request, &RequestMetaObject::default())?;
        let negotiated = self.negotiate_initialize(&request)?;
        request.protocol_version = negotiated.protocol_version;
        Ok(request)
    }
}

impl ServerHandler for OrbitToolServer {
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        if request.method == crate::internal_drain::PREFLIGHT_METHOD && self.internal_drain {
            return Ok(CustomResult(
                serde_json::json!({"protocol": crate::INTERNAL_DRAIN_PROTOCOL}),
            ));
        }
        if request.method != crate::internal_drain::CALL_METHOD {
            return Err(McpError::new(
                rmcp::model::ErrorCode::METHOD_NOT_FOUND,
                request.method,
                None,
            ));
        }
        let params = request.params.unwrap_or(Value::Null);
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .and_then(crate::internal_drain_name)
            .ok_or_else(|| McpError::invalid_params("unknown internal drain operation", None))?;
        let input = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let enabled = self.internal_drain
            && params.get("protocol").and_then(Value::as_u64)
                == Some(crate::INTERNAL_DRAIN_PROTOCOL);
        let context = self.context_for_tool_call();
        let host = Arc::clone(&self.host);
        let result = tokio::task::spawn_blocking(move || {
            if enabled {
                host.call_internal_drain(name, input, context)
            } else {
                host.refuse_internal_drain(name, input, context)
            }
        })
        .await
        .map_err(|error| McpError::internal_error(error.to_string(), None))?;
        let result = match result {
            Ok(value) => mcp_tool_call_result(value),
            Err(error) => tool_error_result(&error),
        };
        Ok(CustomResult(serde_json::to_value(result).map_err(
            |error| McpError::internal_error(error.to_string(), None),
        )?))
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<InitializeResult, McpError>> + Send + '_ {
        if let Err(error) = self.apply_initialize(&request, &context.meta) {
            return std::future::ready(Err(error));
        }
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request.clone());
        }
        std::future::ready(self.negotiate_initialize(&request))
    }

    fn get_info(&self) -> ServerConfig {
        let implementation = Implementation::new("orbit-mcp", env!("CARGO_PKG_VERSION"));
        let capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
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
        let surface = self
            .load_tool_surface()
            .map_err(invalid_definitions_mcp_error)?;
        let advertisement = self.selector_advertisement();
        let cache_key = (advertisement, self.session_context().workspace);
        if let Ok(cache) = surface.lists.lock()
            && let Some(cached) = cache.get(&cache_key)
        {
            return self.without_hidden_tools(
                (**cached).clone(),
                &surface.definitions,
                &surface.name_map,
            );
        }

        // Build and validate the map on the same first pass as the list. This
        // keeps malformed advertised names from being hidden by the response
        // cache while avoiding a rebuild on later calls.
        surface.name_map.clone()?;
        let mut definitions = surface.definitions.as_ref().clone();
        definitions.sort_by(|left, right| left.schema.name.cmp(&right.schema.name));
        // Prose names other tools by their canonical dotted id; the client can
        // only call the advertised alias, so say that in what it reads.
        let mut canonical_names = definitions
            .iter()
            .map(|definition| definition.schema.name.clone())
            .collect::<Vec<_>>();
        canonical_names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        let canonical_names = canonical_names
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let presentation_available =
            self.host.presentation_enabled() && presentation::available(&definitions);
        let mut tools = definitions
            .into_iter()
            .map(|mut definition| {
                let mut input_schema = self
                    .input_schema_for(&definition)
                    .map_err(invalid_definitions_mcp_error)?;
                advertise_tool_names_in_schema(&mut input_schema, &canonical_names);
                definition.schema.description = super::schema::advertised_tool_description(
                    &definition.schema.name,
                    &advertise_tool_names(&definition.schema.description, &canonical_names),
                );
                Ok(schema_to_tool(
                    definition.schema,
                    input_schema,
                    definition.annotations,
                ))
            })
            .collect::<Result<Vec<_>, McpError>>()?;
        if presentation_available {
            tools.extend(presentation::tools(
                self.host.federated_workspace_selectors(),
            ));
            tools.sort_by(|left, right| left.name.cmp(&right.name));
        }
        let result = ListToolsResult::with_all_items(tools);
        if let Ok(mut cache) = surface.lists.lock() {
            cache.insert(cache_key, Arc::new(result.clone()));
        }
        self.without_hidden_tools(result, &surface.definitions, &surface.name_map)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            presentation::resource(),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        if request.uri != presentation::RESOURCE_URI
            && request.uri != presentation::LEGACY_RESOURCE_URI
        {
            return Err(McpError::resource_not_found(
                "unknown UI resource",
                Some(serde_json::json!({ "code": "resource_not_found" })),
            ));
        }
        Ok(ReadResourceResponse::Complete(ReadResourceResult::new(
            vec![presentation::resource_content(&request.uri)],
        )))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        Ok(CallToolResponse::Complete(
            self.dispatch_tool_call(request).await?,
        ))
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
    transport_meta: &RequestMetaObject,
) -> ToolSessionContext {
    // rmcp moves wire `_meta` to the request context. Prefer params-level meta
    // for in-process callers, then fall back to the transport-level value.
    let workspace = meta_string(request.meta.as_ref().map(|meta| &meta.0), "workspace")
        .or_else(|| meta_string(Some(&transport_meta.0), "workspace"));

    ToolSessionContext {
        workspace,
        self_reported_actor: self_reported_actor_from_initialize(request, transport_meta),
        mcp_client_name: Some(request.client_info.name.trim().to_string())
            .filter(|name| !name.is_empty()),
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
    transport_meta: &RequestMetaObject,
) -> Option<String> {
    meta_string(request.meta.as_ref().map(|meta| &meta.0), "actor")
        .or_else(|| meta_string(Some(&transport_meta.0), "actor"))
        .or_else(|| Some(request.client_info.name.clone()))
        .as_deref()
        .and_then(normalize_self_reported_actor)
}

/// Read `_meta.orbit.<key>`, accepting both the nested object and the flat
/// dotted spelling clients use when they cannot build nested `_meta`.
fn meta_string(meta: Option<&MetaObject>, key: &str) -> Option<String> {
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
