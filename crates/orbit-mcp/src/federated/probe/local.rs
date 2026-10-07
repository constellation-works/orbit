//! In-process delivery and selection between local and remote probes.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::{ToolSessionContext, mcp_advertised_tool_name};
use serde_json::{Value, json};

use super::super::config::Destination;
use super::contracts::{DestinationProbe, DestinationSnapshot, RoutedSession};
use super::discovery::snapshot_from_discovery_content;

/// In-process probe for the accepting machine: list and route through the same
/// local [`crate::McpHost`] the v1 MCP surface uses, so local selectors never
/// spawn SSH.
pub struct InProcessDestinationProbe {
    inner: Arc<dyn crate::McpHost>,
    session_context: ToolSessionContext,
}

impl InProcessDestinationProbe {
    pub fn new(inner: Arc<dyn crate::McpHost>, session_context: ToolSessionContext) -> Self {
        Self {
            inner,
            session_context,
        }
    }
}

impl InProcessDestinationProbe {
    fn discover(
        &self,
        destination: &Destination,
        arguments: Value,
    ) -> Result<DestinationSnapshot, OrbitError> {
        let content = self.inner.call_tool(
            crate::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL,
            arguments,
            self.session_context.clone(),
        )?;
        snapshot_from_discovery_content(destination, &content)
    }
}

/// Private discovery arguments that ask a destination for crews.
pub(super) fn crews_arguments() -> Value {
    json!({ "include": [crate::WORKSPACE_LIST_INCLUDE_CREWS] })
}

impl DestinationProbe for InProcessDestinationProbe {
    fn open_internal_drain_route(
        &self,
        destination: &Destination,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        self.open_route(destination)
    }

    fn refuse_internal_drain(
        &self,
        name: &str,
        input: Value,
        call_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let mut context = self.session_context.clone();
        context.trace_id = call_context.trace_id;
        context.self_reported_actor = call_context.self_reported_actor;
        self.inner.refuse_internal_drain(name, input, context)
    }
    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        self.discover(destination, json!({}))
    }

    fn probe_with_crews(
        &self,
        destination: &Destination,
    ) -> Result<DestinationSnapshot, OrbitError> {
        self.discover(destination, crews_arguments())
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        let snapshot = self.probe(destination)?;
        Ok(Box::new(InProcessRoutedSession {
            inner: Arc::clone(&self.inner),
            session_context: self.session_context.clone(),
            snapshot,
        }))
    }
}

struct InProcessRoutedSession {
    inner: Arc<dyn crate::McpHost>,
    /// Trusted server-owned fields captured before initialize. Per-call audit
    /// evidence is overlaid at dispatch and never written back here.
    session_context: ToolSessionContext,
    snapshot: DestinationSnapshot,
}

impl RoutedSession for InProcessRoutedSession {
    fn internal_drain_protocol(&mut self) -> Result<bool, OrbitError> {
        Ok(true)
    }

    fn call_internal_drain(
        &mut self,
        name: &str,
        arguments: Value,
        call_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let mut context = self.session_context.clone();
        context.trace_id = call_context.trace_id;
        context.self_reported_actor = call_context.self_reported_actor;
        self.inner.call_internal_drain(name, arguments, context)
    }
    fn snapshot(&mut self) -> Result<DestinationSnapshot, OrbitError> {
        Ok(self.snapshot.clone())
    }

    fn advertised_tools(&mut self) -> Result<Vec<String>, OrbitError> {
        Ok(self
            .inner
            .list_mcp_tool_definitions()?
            .into_iter()
            .map(|definition| mcp_advertised_tool_name(&definition.schema.name))
            .collect())
    }

    fn supports_tool_argument(&mut self, name: &str, argument: &str) -> Result<bool, OrbitError> {
        Ok(self
            .inner
            .list_mcp_tool_definitions()?
            .iter()
            .any(|definition| {
                definition.schema.name == name
                    && definition
                        .schema
                        .parameters
                        .iter()
                        .any(|parameter| parameter.name == argument)
            }))
    }

    fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
        call_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let mut destination_context = self.session_context.clone();
        if destination_context.worker_invocation.is_some()
            && destination_context.worker_invocation != call_context.worker_invocation
        {
            return Err(OrbitError::PolicyDenied(
                "worker routed binding mismatch".into(),
            ));
        }
        destination_context.worker_invocation = call_context.worker_invocation;
        destination_context.worker_host_call = call_context.worker_host_call;
        if destination_context.worker_invocation.is_some() {
            destination_context
                .effective_capabilities
                .remove(&orbit_types::tool::McpCapability::Operator);
        }
        destination_context.trace_id = call_context.trace_id;
        destination_context.self_reported_actor = call_context.self_reported_actor;
        self.inner.call_tool(name, arguments, destination_context)
    }
}

/// Dispatch local destinations to an in-process probe and remotes to SSH.
pub struct CompositeDestinationProbe {
    local: Arc<dyn DestinationProbe>,
    remote: Arc<dyn DestinationProbe>,
}

impl CompositeDestinationProbe {
    pub fn new(local: Arc<dyn DestinationProbe>, remote: Arc<dyn DestinationProbe>) -> Self {
        Self { local, remote }
    }

    fn probe_for(&self, destination: &Destination) -> &dyn DestinationProbe {
        if destination.is_local() {
            &*self.local
        } else {
            &*self.remote
        }
    }
}

impl DestinationProbe for CompositeDestinationProbe {
    fn open_internal_drain_route(
        &self,
        destination: &Destination,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        self.probe_for(destination)
            .open_internal_drain_route(destination)
    }

    fn refuse_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.local.refuse_internal_drain(name, input, context)
    }
    fn open_worker_route(
        &self,
        destination: &Destination,
        context: &ToolSessionContext,
    ) -> Result<Box<dyn RoutedSession>, OrbitError> {
        self.probe_for(destination)
            .open_worker_route(destination, context)
    }

    fn probe(&self, destination: &Destination) -> Result<DestinationSnapshot, OrbitError> {
        self.probe_for(destination).probe(destination)
    }

    fn probe_with_crews(
        &self,
        destination: &Destination,
    ) -> Result<DestinationSnapshot, OrbitError> {
        self.probe_for(destination).probe_with_crews(destination)
    }

    fn open_route(&self, destination: &Destination) -> Result<Box<dyn RoutedSession>, OrbitError> {
        self.probe_for(destination).open_route(destination)
    }
}
