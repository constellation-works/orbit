use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_tools::OwnerCoordinator;
use orbit_types::tool::{McpCapability, ToolSessionContext, WorkerInvocation};
use serde_json::Value;

use crate::OrbitRuntime;

impl OrbitRuntime {
    pub fn current_worker_invocation(
        global_root: &std::path::Path,
    ) -> Result<Option<WorkerInvocation>, OrbitError> {
        restore_process_binding(global_root).map(|binding| binding.map(|value| (*value).clone()))
    }

    /// Bind a runtime clone to an admitted worker. Composition must obtain the
    /// binding from its managed invocation, never editable job input or env.
    pub fn with_worker_invocation(
        mut self,
        invocation: WorkerInvocation,
        coordinator: Arc<dyn OwnerCoordinator>,
    ) -> Result<Self, OrbitError> {
        invocation.validate().map_err(OrbitError::InvalidInput)?;
        if self
            .worker_invocation
            .as_deref()
            .is_some_and(|old| old != &invocation)
        {
            return Err(OrbitError::PolicyDenied(
                "worker binding is immutable".into(),
            ));
        }
        self.worker_invocation = Some(Arc::new(invocation));
        self.owner_coordinator = Some(coordinator);
        Ok(self)
    }

    pub(crate) fn without_worker_routing(&self) -> Self {
        let mut owner = self.clone();
        owner.worker_invocation = None;
        owner.owner_coordinator = None;
        owner
    }

    pub fn with_owner_coordinator(mut self, coordinator: Arc<dyn OwnerCoordinator>) -> Self {
        self.owner_coordinator = Some(coordinator);
        self
    }

    /// Install the transport a follower drain reaches its owner through
    /// [ORB-13625]. Composition supplies it; nothing in a payload can.
    pub fn with_drain_owner_transport(
        mut self,
        transport: Arc<dyn orbit_tools::DrainOwnerTransport>,
    ) -> Self {
        self.drain_owner_transport = Some(transport);
        self
    }

    pub(crate) fn drain_owner_transport(
        &self,
    ) -> Option<&Arc<dyn orbit_tools::DrainOwnerTransport>> {
        self.drain_owner_transport.as_ref()
    }

    pub(crate) fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        if let Some(binding) = self.worker_invocation() {
            super::recovery_authority::RecoveryAuthority::open(&self.global_root())?
                .bind_worker_process(pid, binding)?;
        }
        Ok(())
    }

    pub fn worker_invocation(&self) -> Option<&WorkerInvocation> {
        self.worker_invocation.as_deref()
    }

    pub(crate) fn read_owner<T: serde::de::DeserializeOwned>(
        &self,
        id: &str,
        projection: &str,
    ) -> Result<T, OrbitError> {
        self.read_owner_request(serde_json::json!({"id": id, "_worker_read": projection}))
    }

    pub(crate) fn read_owner_request<T: serde::de::DeserializeOwned>(
        &self,
        request: Value,
    ) -> Result<T, OrbitError> {
        let value = self.route_worker_tool("orbit.task.show", request, Default::default())?;
        decode_owner_read(value)
    }

    pub(crate) fn bind_worker_session(
        &self,
        session: &mut ToolSessionContext,
    ) -> Result<(), OrbitError> {
        if let Some(bound) = self.worker_invocation() {
            if session
                .worker_invocation
                .as_ref()
                .is_some_and(|provided| provided != bound)
            {
                return Err(OrbitError::PolicyDenied(
                    "worker session binding mismatch".into(),
                ));
            }
            session.worker_invocation = Some(bound.clone());
        }
        if session.worker_invocation.is_some() {
            session
                .effective_capabilities
                .remove(&McpCapability::Operator);
        }
        Ok(())
    }

    pub(crate) fn route_worker_tool(
        &self,
        name: &str,
        mut input: Value,
        mut session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        let bound = self
            .worker_invocation()
            .ok_or_else(|| OrbitError::PolicyDenied("worker binding missing".into()))?;
        self.bind_worker_session(&mut session)?;
        if let Some(value) = input.get("workspace") {
            let valid = value.as_str().is_some_and(|value| {
                value == bound.owner_destination
                    || value == bound.owner_workspace_id
                    || value == self.paths().repo_root.to_string_lossy()
            });
            if !valid {
                return Err(OrbitError::PolicyDenied(
                    "worker workspace binding mismatch".into(),
                ));
            }
        }
        if let Some(object) = input.as_object_mut() {
            object.insert(
                "workspace".into(),
                Value::String(bound.owner_destination.clone()),
            );
        }
        session.workspace = Some(bound.owner_destination.clone());
        self.owner_coordinator
            .as_ref()
            .ok_or_else(|| OrbitError::PolicyDenied("owner destination unavailable".into()))?
            .call(name, input, session)
    }
}

/// Decode an owner read into the type the reader asked for.
///
/// An in-process owner returns the projection as-is. A routed owner answers
/// over MCP, whose `structuredContent` must be an object, so a list arrives as
/// `{"items": [...]}` and a scalar as `{"value": ...}` [ORB-13625]. The value
/// is parsed as sent first, so a projection that genuinely is an object never
/// loses a field; only when that fails is the single-key transport envelope
/// unwrapped.
fn decode_owner_read<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, OrbitError> {
    let direct = match serde_json::from_value::<T>(value.clone()) {
        Ok(decoded) => return Ok(decoded),
        Err(error) => error,
    };
    if let Value::Object(object) = &value
        && object.len() == 1
        && let Some(inner) = object.get("items").or_else(|| object.get("value"))
        && let Ok(decoded) = serde_json::from_value::<T>(inner.clone())
    {
        return Ok(decoded);
    }
    Err(OrbitError::Store(format!("owner read response: {direct}")))
}

pub(crate) fn is_coordination_tool(name: &str) -> bool {
    name.starts_with("orbit.task.")
        || name.starts_with("orbit.friction.")
        || matches!(name, "orbit.auto_task.add" | "orbit.auto_task.update")
}

/// A required child can start before the parent has recorded its PID. Wait
/// briefly for that record; never use environment payload as a substitute.
pub(super) fn restore_process_binding(
    global_root: &std::path::Path,
) -> Result<Option<Arc<WorkerInvocation>>, OrbitError> {
    let required = std::env::var_os("ORBIT_WORKER_CONTEXT_REQUIRED").is_some();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(binding) = super::recovery_authority::current_worker_binding(global_root)? {
            return Ok(Some(Arc::new(binding)));
        }
        if !required {
            return Ok(None);
        }
        if std::time::Instant::now() >= deadline {
            return Err(OrbitError::PolicyDenied(
                "managed worker runtime binding unavailable".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
