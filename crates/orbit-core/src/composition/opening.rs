//! [`Opening`]: one workspace while composition assembles it, before it
//! becomes the [`Core`] a surface holds.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_common::{Env, OrbitError};
use orbit_types::task::{ExecutionLocation, HostOs};
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use serde_json::Value;

use super::core::{Core, core_for_command, core_for_service};
use crate::ActorIdentity;
use crate::runtime::workspace::catalog::WorkspaceCatalog;
use crate::runtime::{
    HostLifetime, OrbitRuntime, OrbitRuntimeRoots, RuntimeKernel, WorkspaceRootHint,
    WorkspaceRuntimeBinding,
};

/// One workspace the composition tier is opening.
///
/// The composition tier above this crate resolves which checkout to open and
/// what the registry knows about it, then attaches those facts here. Its only
/// exits are a [`Core`] and, for a kernel interface that returns one, the
/// kernel. It offers the open modes, the facts composition attaches and the
/// few reads composition needs to decide what to attach; no operation.
#[derive(Clone)]
pub struct Opening {
    runtime: OrbitRuntime,
}

impl Opening {
    /// Open `roots` for writing, projecting `task_prefix` onto the task
    /// registry this open creates.
    pub fn initialize(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
        task_prefix: Option<&str>,
    ) -> Result<Self, OrbitError> {
        OrbitRuntime::initialize_from_resolved_roots_with_task_prefix(roots, binding, task_prefix)
            .map(Self::of)
    }

    /// Open `roots` for an observation command, without stale-run
    /// reconciliation.
    pub fn initialize_read_only(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
    ) -> Result<Self, OrbitError> {
        OrbitRuntime::initialize_from_resolved_roots_read_only(roots, binding).map(Self::of)
    }

    /// Open `roots` for an inspection report that creates, migrates and
    /// repairs nothing.
    pub fn initialize_observational(
        roots: OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
    ) -> Result<Self, OrbitError> {
        OrbitRuntime::initialize_from_resolved_roots_observational(roots, binding).map(Self::of)
    }

    /// Open resolved roots with no registered binding.
    pub fn unbound(
        roots: &OrbitRuntimeRoots,
        task_prefix: Option<&str>,
    ) -> Result<Self, OrbitError> {
        OrbitRuntime::from_resolved_roots_with_task_prefix(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            task_prefix,
        )
        .map(Self::of)
    }

    /// Open resolved roots under a registered binding.
    pub fn bound(
        roots: &OrbitRuntimeRoots,
        binding: WorkspaceRuntimeBinding,
        host_lifetime: HostLifetime,
        task_prefix: Option<&str>,
    ) -> Result<Self, OrbitError> {
        OrbitRuntime::from_resolved_roots_with_binding_for_task_prefix(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            binding,
            host_lifetime,
            task_prefix,
        )
        .map(Self::of)
    }

    /// Open resolved roots under a registered binding for an observation
    /// command.
    pub fn bound_read_only(
        roots: &OrbitRuntimeRoots,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        OrbitRuntime::from_resolved_roots_read_only_with_binding(
            &roots.global_root,
            &roots.shared_root,
            &roots.local_root,
            binding,
        )
        .map(Self::of)
    }

    /// Open a registered checkout's existing state with no incidental writes.
    pub fn bound_observational(
        global_root: &Path,
        shared_root: &Path,
        binding: WorkspaceRuntimeBinding,
    ) -> Result<Self, OrbitError> {
        OrbitRuntime::from_resolved_roots_observational_with_binding(
            global_root,
            shared_root,
            binding,
        )
        .map(Self::of)
    }

    fn of(runtime: OrbitRuntime) -> Self {
        Self { runtime }
    }

    /// The roots a command opens from `cwd`, with the catalog `hint`.
    pub fn resolve_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
        hint: Option<&WorkspaceRootHint>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        OrbitRuntime::resolve_roots_for_cwd_with_hint(cwd, root_override, hint)
    }

    /// The roots a bootstrap command (`init`, `migrate`) opens from `cwd`,
    /// with the catalog `hint`.
    pub fn resolve_bootstrap_roots_for_cwd(
        cwd: &Path,
        root_override: Option<&Path>,
        hint: Option<&WorkspaceRootHint>,
    ) -> Result<OrbitRuntimeRoots, OrbitError> {
        OrbitRuntime::resolve_bootstrap_roots_for_cwd_with_hint(cwd, root_override, hint)
    }

    /// The worker binding recorded under `global_root`, read before any
    /// workspace opens.
    pub fn current_worker_invocation(
        env: &Env,
        global_root: &Path,
    ) -> Result<Option<WorkerInvocation>, OrbitError> {
        OrbitRuntime::current_worker_invocation(env, global_root)
    }

    pub fn with_actor(self, actor: ActorIdentity) -> Self {
        self.map(|runtime| runtime.with_actor(actor))
    }

    pub fn with_coordination_write_owner(self, owner_machine_id: Option<String>) -> Self {
        self.map(|runtime| runtime.with_coordination_write_owner(owner_machine_id))
    }

    pub fn with_workspace_explicitly_selected(self, selected: bool) -> Self {
        self.map(|runtime| runtime.with_workspace_explicitly_selected(selected))
    }

    pub fn with_automation_execution_location(self, location: Option<ExecutionLocation>) -> Self {
        self.map(|runtime| runtime.with_automation_execution_location(location))
    }

    pub fn with_opened_registered_workspace(self, identity: Option<(String, String)>) -> Self {
        self.map(|runtime| runtime.with_opened_registered_workspace(identity))
    }

    pub fn with_composition_snapshot(self, snapshot: Arc<dyn std::any::Any + Send + Sync>) -> Self {
        self.map(|runtime| runtime.with_composition_snapshot(snapshot))
    }

    pub fn with_workspace_catalog(self, catalog: Arc<dyn WorkspaceCatalog>) -> Self {
        self.map(|runtime| runtime.with_workspace_catalog(catalog))
    }

    pub fn with_registered_host_oses(
        self,
        hosts: impl IntoIterator<Item = (String, HostOs)>,
    ) -> Self {
        self.map(|runtime| runtime.with_registered_host_oses(hosts))
    }

    pub fn with_owner_coordinator(
        self,
        coordinator: Arc<dyn orbit_tools::OwnerCoordinator>,
    ) -> Self {
        self.map(|runtime| runtime.with_owner_coordinator(coordinator))
    }

    pub fn with_drain_owner_transport(
        self,
        transport: Arc<dyn orbit_tools::DrainOwnerTransport>,
    ) -> Self {
        self.map(|runtime| runtime.with_drain_owner_transport(transport))
    }

    /// The opened checkout's repository root.
    pub fn repo_root(&self) -> &Path {
        &self.runtime.paths().repo_root
    }

    pub fn global_root(&self) -> PathBuf {
        self.runtime.global_root()
    }

    /// Whether this process runs as an admitted worker.
    pub fn is_worker(&self) -> bool {
        self.runtime.role().is_worker()
    }

    /// The machine id automation records for this process, once attached.
    pub fn automation_machine_identity(&self) -> Option<&str> {
        self.runtime.automation_machine_identity()
    }

    /// Run a worker's coordination call in this process, which owns it: the
    /// in-process owner route composition attaches to a worker.
    pub fn execute_owner_coordination(
        &self,
        name: &str,
        input: Value,
        session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.runtime
            .execute_owner_coordination(name, input, session)
    }

    /// The `Core` of a process that runs one command and exits.
    pub fn into_command_core(self) -> Core {
        core_for_command(self.runtime)
    }

    /// The `Core` a long-lived server keeps for this workspace.
    pub fn into_service_core(self) -> Core {
        core_for_service(self.runtime)
    }

    /// The kernel, for a kernel interface that returns one.
    pub fn into_kernel(self) -> RuntimeKernel {
        self.runtime.into_kernel()
    }

    /// The runtime, for a downstream crate's tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn into_runtime(self) -> OrbitRuntime {
        self.runtime
    }

    fn map(self, attach: impl FnOnce(OrbitRuntime) -> OrbitRuntime) -> Self {
        Self::of(attach(self.runtime))
    }
}

impl crate::application::routines::DiscoveredWorkspaces {
    /// Add an owner checkout composition opened for a routine sweep.
    pub fn push_entry(&mut self, workspace: orbit_types::workspace::Workspace, opened: Opening) {
        self.entries.push((workspace, opened.runtime));
    }

    /// Add a replica checkout composition opened for a routine sweep.
    pub fn push_replica(&mut self, workspace: orbit_types::workspace::Workspace, opened: Opening) {
        self.replicas.push((workspace, opened.runtime));
    }
}
