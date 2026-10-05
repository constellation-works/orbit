//! The federated mux host: one MCP surface over many configured destinations.

use std::str::FromStr;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::{
    McpToolAnnotations, McpToolDefinition, McpToolScope, ToolSchema, ToolSessionContext,
    mcp_advertised_tool_name,
};
use orbit_types::workspace::WorkspaceStatus;
use serde_json::{Map, Value, json};

use super::config::{Destination, MachineQualifiedSelector};
use super::descriptor::WorkspaceDescriptor;
use super::probe::{DestinationProbe, DestinationSnapshot};

/// Federated discovery stays session-unbound and is answered by the mux.
///
/// Every other advertised tool is delivered to the destination encoded in the
/// caller's host-qualified selector.
pub const FEDERATED_WORKSPACE_LIST_TOOL: &str = "orbit.workspace.list";

/// An MCP host that aggregates operator-configured destinations.
pub struct FederatedMcpHost {
    destinations: Vec<Destination>,
    probe: Arc<dyn DestinationProbe>,
}

impl FederatedMcpHost {
    pub fn new(destinations: Vec<Destination>, probe: Arc<dyn DestinationProbe>) -> Self {
        Self {
            destinations,
            probe,
        }
    }

    /// The federated list, in configured order.
    ///
    /// `machine_id` is on each descriptor, not the envelope: one response now
    /// spans many machines, so a single envelope-level identity would be a lie
    /// about all but one of them.
    /// With `include: ["crews"]` each reachable row also carries the crews its
    /// own destination resolved, so a remote workspace never borrows the
    /// accepting machine's crew configuration.
    fn list_workspaces(&self, input: &Value) -> Result<Value, OrbitError> {
        let include_crews = crate::workspace_list_includes_crews(input)?;
        Ok(json!({ "workspaces": self.probe_all_destinations(include_crews) }))
    }

    /// Probe every destination concurrently, so the list costs the slowest
    /// destination rather than the sum of all of them.
    fn probe_all_destinations(&self, include_crews: bool) -> Vec<WorkspaceDescriptor> {
        std::thread::scope(|scope| {
            let probes = self
                .destinations
                .iter()
                .map(|destination| {
                    (
                        destination,
                        scope.spawn(move || self.describe_destination(destination, include_crews)),
                    )
                })
                .collect::<Vec<_>>();
            probes
                .into_iter()
                .flat_map(|(destination, probe)| {
                    probe.join().unwrap_or_else(|_| {
                        tracing::warn!(
                            machine_id = %destination.machine_id,
                            "federated destination probe panicked",
                        );
                        vec![WorkspaceDescriptor::unreachable(destination)]
                    })
                })
                .collect()
        })
    }

    /// One destination's rows. Never empty: a configured destination the caller
    /// cannot see is worse than one it can see is down.
    fn describe_destination(
        &self,
        destination: &Destination,
        include_crews: bool,
    ) -> Vec<WorkspaceDescriptor> {
        let probed = if include_crews {
            self.probe.probe_with_crews(destination)
        } else {
            self.probe.probe(destination)
        };
        let mut snapshot = match probed {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(
                    machine_id = %destination.machine_id,
                    %error,
                    "federated destination did not answer its live probe",
                );
                return vec![WorkspaceDescriptor::unreachable(destination)];
            }
        };
        if let Err(error) = confirm_pinned_identity(destination, &snapshot) {
            // A destination answering under a different identity is not a new
            // error class: whatever answered, the configured machine did not.
            tracing::warn!(
                machine_id = %destination.machine_id,
                %error,
                "federated destination answered under a different machine_id",
            );
            return vec![WorkspaceDescriptor::unreachable(destination)];
        }
        if snapshot.workspaces.is_empty() {
            return vec![WorkspaceDescriptor::workspaceless(destination)];
        }
        let workspaces = std::mem::take(&mut snapshot.workspaces);
        workspaces
            .into_iter()
            .map(|workspace| {
                let crews = snapshot.crews.remove(&workspace.id).unwrap_or_default();
                WorkspaceDescriptor::reachable(destination, workspace).with_crews(crews)
            })
            .collect()
    }

    /// Deliver a workspace-scoped call to the destination encoded in the selector.
    ///
    /// Classification uses a live session, not the last list. Fail-closed
    /// precedence: unknown selector, then unreachable, stale, unhealthy,
    /// tool-not-on-this-host, then the destination's own refusal.
    ///
    /// That precedence covers everything decidable *before* dispatch. The
    /// delivery itself runs on its own budget and, if its answer is lost, is
    /// reported as `outcome_unknown` rather than re-entering this ladder.
    fn route_workspace_call(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
        internal: bool,
    ) -> Result<Value, OrbitError> {
        let token = workspace_selector(&input, &session_context).ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "tool '{name}' requires a host-qualified workspace selector; copy `selector` from \
                 federated orbit.workspace.list"
            ))
        })?;
        // Parse before opening a destination session: a bare `ws_*`, display
        // form, or any other non-host-qualified token is unknown_selector, even
        // when initialize injected the v1 local default.
        let parsed = MachineQualifiedSelector::from_str(token)?;
        if let Some(binding) = &session_context.worker_invocation {
            binding.validate().map_err(OrbitError::InvalidInput)?;
            if token != binding.owner_destination || parsed.machine_id() != binding.owner_machine_id
            {
                return Err(OrbitError::PolicyDenied(
                    "worker owner selector mismatch".into(),
                ));
            }
        }
        let destination = self
            .destinations
            .iter()
            .find(|destination| destination.machine_id == parsed.machine_id())
            .ok_or_else(|| OrbitError::UnknownSelector(token.to_string()))?;

        let mut session = if internal {
            self.probe.open_internal_drain_route(destination)
        } else {
            self.probe.open_worker_route(destination, &session_context)
        }
        .map_err(|error| delivery_unreachable(destination, error))?;
        let snapshot = session
            .snapshot()
            .map_err(|error| delivery_unreachable(destination, error))?;
        if let Err(error) = confirm_pinned_identity(destination, &snapshot) {
            tracing::warn!(
                machine_id = %destination.machine_id,
                %error,
                "federated destination answered under a different machine_id",
            );
            return Err(error);
        }
        let Some(workspace) = snapshot
            .workspaces
            .iter()
            .find(|workspace| workspace.id == parsed.workspace_id())
        else {
            return Err(OrbitError::StaleRoute(token.to_string()));
        };
        if workspace.status == WorkspaceStatus::Invalid {
            return Err(OrbitError::UnhealthyCheckout(token.to_string()));
        }

        if internal {
            if !session.internal_drain_protocol()? {
                return Err(OrbitError::ToolNotOnThisHost(
                    "destination does not support the internal drain protocol".into(),
                ));
            }
        } else {
            let advertised = session
                .advertised_tools()
                .map_err(|error| delivery_unreachable(destination, error))?;
            for argument in [
                "view",
                "snapshot",
                "request_id",
                "expected_revision",
                "verdict",
                "complete",
                "approve_proposed",
                "expected_enabled",
                "acknowledge_unconditional",
                "default_input",
                "include_catalog",
            ] {
                if input.get(argument).is_some()
                    && !session.supports_tool_argument(name, argument)?
                {
                    return Err(OrbitError::ToolNotOnThisHost(format!(
                        "'{name}' argument '{argument}' is unsupported on '{}'",
                        destination.machine_id
                    )));
                }
            }
            if !tool_on_surface(&advertised, name) {
                return Err(OrbitError::ToolNotOnThisHost(format!(
                    "'{name}' is not advertised on '{}'",
                    destination.machine_id
                )));
            }
        }

        tracing::info!(
            machine_id = %destination.machine_id,
            workspace_id = %parsed.workspace_id(),
            tool = name,
            "federated mux delivering tool call"
        );
        // Not wrapped by `delivery_unreachable`: past this point the request
        // reaches the destination, so its failure is the destination's answer
        // (`RemoteTool`) or a post-dispatch ambiguity (`OutcomeUnknown`) —
        // never a delivery miss the caller should retry [ORB-11023].
        let arguments = destination_arguments(input, parsed.workspace_id());
        if internal {
            session.call_internal_drain(name, arguments, session_context)
        } else {
            session.call_tool(name, arguments, session_context)
        }
    }

    /// Owner/follower route, independent of public tools/list and tools/call.
    pub fn call_internal_drain(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if session.worker_invocation.is_some() {
            return Err(crate::internal_drain::refusal());
        }
        let canonical =
            crate::internal_drain_name(name).ok_or_else(crate::internal_drain::refusal)?;
        self.route_workspace_call(canonical, input, session, true)
    }
}

impl crate::McpHost for FederatedMcpHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        let mut definitions = crate::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        for definition in &mut definitions {
            if definition.schema.name == FEDERATED_WORKSPACE_LIST_TOOL {
                *definition = federated_workspace_list_definition();
            }
        }
        Ok(definitions)
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if let Some(canonical) = crate::internal_drain_name(name) {
            return self.refuse_internal_drain(canonical, input, session_context);
        }
        if name == FEDERATED_WORKSPACE_LIST_TOOL {
            return self.list_workspaces(&input);
        }
        self.route_workspace_call(name, input, session_context, false)
    }

    fn refuse_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.probe.refuse_internal_drain(name, input, context)
    }

    fn federated_workspace_selectors(&self) -> bool {
        true
    }
}

impl orbit_tools::OwnerCoordinator for FederatedMcpHost {
    fn call(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if session.worker_invocation.is_none() {
            return Err(OrbitError::PolicyDenied(
                "owner route requires worker binding".into(),
            ));
        }
        self.route_workspace_call(name, input, session, false)
    }
}

/// The federated list definition.
///
/// [`McpToolScope::Global`] is what makes it session-unbound: it takes no
/// workspace selector, and an announced session workspace is neither an input
/// nor a filter. The description is deliberately not v1's — this is a new
/// response shape, not a compatible extension of the machine-local list.
fn federated_workspace_list_definition() -> McpToolDefinition {
    McpToolDefinition::new(
        ToolSchema {
            name: FEDERATED_WORKSPACE_LIST_TOOL.to_string(),
            description: "List the accepting machine's workspaces together with every configured \
                          remote destination's workspaces as live descriptors, including remotes \
                          that are unreachable right now. Copy a row's `selector` to address that \
                          workspace; do not parse or construct it. Pass `include: [\"crews\"]` \
                          for each workspace's crews as its own machine configures them."
                .to_string(),
            parameters: vec![crate::remote::workspace_list_include_param()],
            builtin: true,
        },
        McpToolScope::Global,
    )
    .with_annotations(Some(McpToolAnnotations::READ_ONLY))
}

/// The operator's config pin is the identity of record; a live answer only
/// confirms it.
fn confirm_pinned_identity(
    destination: &Destination,
    snapshot: &DestinationSnapshot,
) -> Result<(), OrbitError> {
    if snapshot.machine_id == destination.machine_id {
        return Ok(());
    }
    Err(OrbitError::UnreachableDestination(format!(
        "'{}' is configured as machine '{}' but answered as '{}'",
        destination.machine_name_display(),
        destination.machine_id,
        snapshot.machine_id
    )))
}

/// The selector the call itself passed, else the session's announced one.
fn workspace_selector<'a>(input: &'a Value, context: &'a ToolSessionContext) -> Option<&'a str> {
    input
        .get("workspace")
        .and_then(Value::as_str)
        .or(context.workspace.as_deref())
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
}

/// v1 destinations address a local `ws_*`, not the host-qualified token.
fn destination_arguments(input: Value, workspace_id: &str) -> Value {
    let mut object = match input {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    object.insert(
        "workspace".to_string(),
        Value::String(workspace_id.to_string()),
    );
    Value::Object(object)
}

fn tool_on_surface(advertised: &[String], name: &str) -> bool {
    let wire = mcp_advertised_tool_name(name);
    advertised
        .iter()
        .any(|tool| tool == name || *tool == wire || mcp_advertised_tool_name(tool) == wire)
}

/// Connect, snapshot, and tools/list failures are delivery misses: capability
/// and stale are undecidable without the host.
///
/// It covers only the classification phases. The routed `tools/call` is not
/// re-labelled here, because once that request is written the call may have
/// run: [`OrbitError::OutcomeUnknown`] is its honest class.
fn delivery_unreachable(destination: &Destination, error: OrbitError) -> OrbitError {
    match error {
        OrbitError::UnreachableDestination(_) => error,
        other => OrbitError::UnreachableDestination(format!("{}: {other}", destination.machine_id)),
    }
}
