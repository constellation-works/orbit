//! Managed-worker owner-coordinator routing and test wait seams on `RuntimeKernel`; imports `orbit_runtime`, `orbit-common`, `orbit-tools`, and `orbit-types`.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_tools::OwnerCoordinator;
use orbit_types::tool::{McpCapability, ToolSessionContext, WorkerInvocation};
use serde_json::Value;

use crate::RuntimeKernel;
use crate::stored::task_access::worker_binding_missing;

#[cfg(feature = "test-support")]
type TestWait = fn(std::time::Duration);

#[cfg(feature = "test-support")]
thread_local! {
    static TEST_PIPELINE_POLL_WAIT: std::cell::Cell<Option<TestWait>> = const {
        std::cell::Cell::new(None)
    };
}

/// Substitute only a pipeline wait's polling sleep on this thread. Elapsed
/// time, caller deadlines, snapshots and child/process operations remain real.
/// Restore the previous callback on return or unwind; worker threads must opt in
/// separately, and cadence/deadline conformance tests must use the system wait.
#[cfg(feature = "test-support")]
pub fn with_pipeline_poll_wait<T>(wait: TestWait, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<TestWait>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_PIPELINE_POLL_WAIT.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(TEST_PIPELINE_POLL_WAIT.with(|slot| slot.replace(Some(wait))));
    operation()
}

#[cfg(feature = "test-support")]
pub fn pipeline_poll_wait_override() -> Option<TestWait> {
    TEST_PIPELINE_POLL_WAIT.with(std::cell::Cell::get)
}

impl RuntimeKernel {
    pub fn current_worker_invocation(
        env: &orbit_common::Env,
        global_root: &std::path::Path,
    ) -> Result<Option<WorkerInvocation>, OrbitError> {
        restore_process_binding(env, global_root)
            .map(|binding| binding.map(|value| (*value).clone()))
    }

    /// Bind a runtime clone to an admitted worker. Composition must obtain the
    /// binding from its managed invocation, never editable job input or env.
    pub fn with_worker_invocation(
        mut self,
        invocation: WorkerInvocation,
        coordinator: Arc<dyn OwnerCoordinator>,
    ) -> Result<Self, OrbitError> {
        invocation.validate()?;
        if self
            .base()
            .role()
            .invocation()
            .is_some_and(|old| old != &invocation)
        {
            return Err(OrbitError::PolicyDenied(
                "worker binding is immutable".into(),
            ));
        }
        let role = crate::base::role::Role::resolve(
            self.base().workspace_binding.as_deref(),
            Some(Arc::new(invocation)),
        );
        self.stored_mut().set_role(role);
        self.stored_mut().set_owner_coordinator(coordinator);
        Ok(self)
    }

    pub fn without_worker_routing(&self) -> Self {
        let mut owner = self.clone();
        let role =
            crate::base::role::Role::resolve(owner.base().workspace_binding.as_deref(), None);
        owner.stored_mut().set_role(role);
        owner
    }

    pub fn with_owner_coordinator(mut self, coordinator: Arc<dyn OwnerCoordinator>) -> Self {
        self.stored_mut().set_owner_coordinator(coordinator);
        self
    }

    /// Install the transport a follower drain reaches its owner through
    /// [ORB-13625]. Composition supplies it; nothing in a payload can.
    pub fn with_drain_owner_transport(
        mut self,
        transport: Arc<dyn orbit_tools::DrainOwnerTransport>,
    ) -> Self {
        self.run_mut().drain_owner_transport = Some(transport);
        self
    }

    pub fn drain_owner_transport(&self) -> Option<&Arc<dyn orbit_tools::DrainOwnerTransport>> {
        self.run().drain_owner_transport.as_ref()
    }

    pub fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        if let Some(binding) = self.worker_invocation() {
            crate::stored::recovery_authority::RecoveryAuthority::open(&self.global_root())?
                .bind_worker_process(pid, binding)?;
        }
        Ok(())
    }

    /// Read a host-owned projection that is not a task read from the owner,
    /// with the task access's lock-busy retry and response decoding. Task
    /// reads go through [`crate::Stored::task`].
    pub fn read_owner_tool_request<T: serde::de::DeserializeOwned>(
        &self,
        tool: &str,
        request: Value,
    ) -> Result<T, OrbitError> {
        if tool == "orbit.search" {
            check_worker_search_scope(&request)?;
        }
        let owner = self
            .stored()
            .owner_route()
            .ok_or_else(worker_binding_missing)?;
        owner.read(&self.paths().repo_root, tool, request)
    }

    pub fn bind_worker_session(&self, session: &mut ToolSessionContext) -> Result<(), OrbitError> {
        if let Some(owner) = self.stored().owner_route() {
            owner.bind_session(session)?;
        }
        if session.worker_invocation.is_some() {
            session
                .effective_capabilities
                .remove(&McpCapability::Operator);
        }
        Ok(())
    }

    pub fn route_worker_tool(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        check_worker_host_input(&input, session.worker_host_call)?;
        if name == "orbit.search" {
            check_worker_search_scope(&input)?;
        }
        let owner = self
            .stored()
            .owner_route()
            .ok_or_else(worker_binding_missing)?;
        owner.call(&self.paths().repo_root, name, input, session)
    }

    /// The host owns internal projections. Ordinary tool dispatch cannot
    /// select this route by adding a key to its input or changing its env.
    pub fn route_worker_host_tool(&self, name: &str, input: Value) -> Result<Value, OrbitError> {
        self.route_worker_tool(
            name,
            input,
            ToolSessionContext {
                worker_host_call: true,
                ..Default::default()
            },
        )
    }
}

pub fn check_worker_host_input(input: &Value, host_call: bool) -> Result<(), OrbitError> {
    if !host_call
        && ["_worker_read", "_worker_update", "_worker_steering"]
            .iter()
            .any(|key| input.get(key).is_some())
    {
        return Err(OrbitError::PolicyDenied(
            "internal worker projections require a host-owned channel".into(),
        ));
    }
    Ok(())
}

/// Search from a claimed worker may read only its bound owner workspace.
/// Check both canonical fields and aliases even if both are supplied: parser
/// precedence must never turn a conflicting alias into a scope escape.
pub fn check_worker_search_scope(input: &Value) -> Result<(), OrbitError> {
    let wider = ["workspaces", "workspace_scope"].iter().any(|key| {
        input.get(key).is_some_and(|value| {
            !value.is_null()
                && !value.as_array().is_some_and(Vec::is_empty)
                && !value.as_str().is_some_and(|s| s.trim().is_empty())
        })
    }) || ["all_workspaces", "allWorkspaces"].iter().any(|key| {
        input.get(key).is_some_and(|value| {
            !value.is_null()
                && value.as_bool() != Some(false)
                && !value
                    .as_str()
                    .is_some_and(|s| s.trim().eq_ignore_ascii_case("false"))
        })
    });
    if wider {
        return Err(OrbitError::PolicyDenied(
            "claimed_owner_bridge_refused: search is limited to the claimed workspace; federated scope is unavailable".into(),
        ));
    }
    Ok(())
}

pub fn is_coordination_tool(name: &str) -> bool {
    orbit_types::tool::tool_policy_operations(name)
        .into_iter()
        .any(is_coordination_operation)
}

fn is_coordination_operation(name: &str) -> bool {
    name.starts_with("orbit.task.")
        || name.starts_with("orbit.friction.")
        || matches!(
            name,
            "orbit.search"
                | "orbit.auto_task.add"
                | "orbit.auto_task.update"
                | "orbit.notice.post"
                | "orbit.notice.show"
                | "orbit.notice.close"
        )
}

/// A required child can start before the parent has recorded its PID. Wait
/// briefly for that record; never use environment payload as a substitute.
pub(crate) fn restore_process_binding(
    env: &orbit_common::Env,
    global_root: &std::path::Path,
) -> Result<Option<Arc<WorkerInvocation>>, OrbitError> {
    let required = env.orbit_worker_context_required().is_some();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(binding) =
            crate::stored::recovery_authority::current_worker_binding(global_root)?
        {
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
