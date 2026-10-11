//! Runtime construction and deterministic reconciliation hooks.

use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_types::workflow::ShipMode;

use super::{HostLifetime, OrbitRuntime};
use crate::runtime::host_resource::{HostResourceMonitor, default_host_resource_probe};
use crate::runtime::host_signal::{FixedHostSignals, default_host_signal_probe};
use crate::runtime::task_pr_forge::{NoTaskPrForge, default_task_pr_forge};
use crate::runtime::workspace::binding::WorkspaceRuntimeBinding;
use crate::runtime::{builder, event_bus, worker_coordination};

impl OrbitRuntime {
    /// Set the observation clock for deterministic CI-failure filing.
    /// Without an override, filing uses the current UTC time.
    pub fn with_ci_failure_time(mut self, now: chrono::DateTime<chrono::Utc>) -> Self {
        self.ci_failure_time = Some(now);
        self
    }

    /// Only the accepting transport's authenticated facts may label artifact bytes.
    pub(crate) fn artifact_origin(
        &self,
        session: &orbit_types::tool::ToolSessionContext,
    ) -> Option<orbit_types::task::ExecutionLocation> {
        if let Some(binding) = &session.worker_invocation {
            return Some(binding.execution.clone());
        }
        // Remote caller labels do not identify artifact authorship. Until trusted
        // claim propagation supplies execution provenance, keep remote origin unknown.
        if session
            .transport
            .is_some_and(|transport| transport != orbit_types::tool::McpTransport::Local)
        {
            return None;
        }
        session
            .process_machine_id
            .as_deref()
            .or_else(|| self.automation_machine_identity())
            .map(|machine_id| orbit_types::task::ExecutionLocation {
                machine_id: machine_id.into(),
                machine_name: session.process_machine_name.clone(),
            })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_from_resolved_config(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: Option<WorkspaceRuntimeBinding>,
        runtime_config: &orbit_config::ResolvedConfig,
        layout_report: orbit_store::workflow::layout::LayoutUpgradeReport,
        host_lifetime: HostLifetime,
        read_only: bool,
    ) -> Result<Self, OrbitError> {
        let access = if read_only {
            builder::StateAccess::ReadOnly
        } else {
            builder::StateAccess::Write
        };
        Self::finish_from_context(
            builder::build_context_from_roots(
                global_root,
                shared_root,
                local_root,
                binding.as_ref(),
                runtime_config,
                host_lifetime,
                access,
            )?,
            binding,
            global_root,
            layout_report,
        )
    }

    pub(crate) fn build_from_resolved_config_write_free(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: Option<WorkspaceRuntimeBinding>,
        runtime_config: &orbit_config::ResolvedConfig,
        layout_report: orbit_store::workflow::layout::LayoutUpgradeReport,
        host_lifetime: HostLifetime,
    ) -> Result<Self, OrbitError> {
        let mut runtime = Self::finish_from_context(
            builder::build_context_from_roots(
                global_root,
                shared_root,
                local_root,
                binding.as_ref(),
                runtime_config,
                host_lifetime,
                builder::StateAccess::WriteFree,
            )?,
            binding,
            global_root,
            layout_report,
        )?;
        runtime.write_free = true;
        Ok(runtime)
    }

    /// Whether this runtime opened Orbit's state without write access because
    /// the process joined a live generation it may not record itself into.
    /// Reads work; operations that would repair or finalize stored state must
    /// be skipped rather than attempted. Tool audit appends use a separate
    /// connection without granting writes to these runtime state handles.
    pub fn is_write_free(&self) -> bool {
        self.write_free
    }

    fn finish_from_context(
        context: crate::context::OrbitContext,
        binding: Option<WorkspaceRuntimeBinding>,
        global_root: &Path,
        layout_report: orbit_store::workflow::layout::LayoutUpgradeReport,
    ) -> Result<Self, OrbitError> {
        let host_resources = Arc::new(
            HostResourceMonitor::new(
                default_host_resource_probe(),
                context.settings().resource_throttle().clone(),
            )
            .with_shared_history(global_root)
            .sampled_in_background(),
        );
        Ok(Self {
            host_resources,
            context,
            write_free: false,
            workspace_binding: binding.map(Arc::new),
            worker_invocation: worker_coordination::restore_process_binding(global_root)?,
            owner_coordinator: None,
            drain_owner_transport: None,
            coordination_write_owner: None,
            automation_execution_location: None,
            host_os: orbit_types::task::HostOs::current(),
            ci_failure_time: None,
            workspace_catalog: None,
            host_signals: default_host_signal_probe(),
            task_pr_forge: default_task_pr_forge(),
            event_log: event_bus::EventLog::default(),
            layout_report: Arc::new(layout_report),
            _temp_dir: None,
            orphan_reconcile_hook: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            transition_read_hook: Arc::new(Mutex::new(None)),
        })
    }

    pub(crate) fn build_in_memory_from_resolved_config(
        data_root: &Path,
        workspace_root: &Path,
        runtime_config: &orbit_config::ResolvedConfig,
        temp_dir: builder::TempDir,
    ) -> Result<Self, OrbitError> {
        // Use a distinct checkout root so the normal workspace builder creates
        // its identity and task partition. A shared explicit data root assumes
        // those were already initialized by workspace init.
        let binding = WorkspaceRuntimeBinding {
            logical_workspace_id: "ws_memory".to_string(),
            task_partition_id: "ws_memory".to_string(),
            owner_machine_id: None,
            checkout_role: None,
            repo_root: data_root.to_path_buf(),
            ship_mode: ShipMode::Local,
            base_branch: None,
        };
        let context = builder::build_context_from_roots(
            data_root,
            workspace_root,
            workspace_root,
            Some(&binding),
            runtime_config,
            HostLifetime::ShortLived,
            builder::StateAccess::Write,
        )?;
        let host_resources = Arc::new(
            HostResourceMonitor::new(
                default_host_resource_probe(),
                context.settings().resource_throttle().clone(),
            )
            .with_shared_history(data_root)
            .sampled_in_background(),
        );
        Ok(Self {
            host_resources,
            context,
            write_free: false,
            workspace_binding: Some(Arc::new(binding)),
            worker_invocation: None,
            owner_coordinator: None,
            drain_owner_transport: None,
            coordination_write_owner: None,
            automation_execution_location: None,
            host_os: orbit_types::task::HostOs::current(),
            ci_failure_time: None,
            workspace_catalog: None,
            // An in-memory runtime is not bound to a host lifecycle.
            host_signals: Arc::new(FixedHostSignals::none()),
            // Nor is it bound to a repository whose PRs it could close.
            task_pr_forge: Arc::new(NoTaskPrForge),
            event_log: event_bus::EventLog::default(),
            layout_report: Arc::new(orbit_store::workflow::layout::LayoutUpgradeReport::default()),
            _temp_dir: Some(Arc::new(temp_dir)),
            orphan_reconcile_hook: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            transition_read_hook: Arc::new(Mutex::new(None)),
        })
    }

    /// Install a one-shot hook run at the start of the next orphaned-leaf
    /// reconciliation on this runtime, before that call reads admissions.
    ///
    /// The pull-refill boundary test uses it to persist a graceful cancel
    /// while reconciliation is in progress. Production never installs a hook.
    pub fn install_orphan_reconcile_hook(&self, hook: impl FnOnce() + Send + 'static) {
        *self
            .orphan_reconcile_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(hook));
    }

    pub(crate) fn run_orphan_reconcile_hook(&self) {
        let hook = self
            .orphan_reconcile_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Outcome of the workspace-layout pre-flight that ran when this runtime
    /// opened: which layout migrations (if any) were auto-applied.
    pub fn layout_upgrade_report(&self) -> &orbit_store::workflow::layout::LayoutUpgradeReport {
        &self.layout_report
    }

    #[cfg(test)]
    pub(crate) fn set_transition_read_hook_status(
        &self,
        id: Option<&str>,
        status: Option<orbit_types::task::TaskStatus>,
    ) {
        *self
            .transition_read_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            id.zip(status).map(|(id, status)| (id.to_string(), status));
    }

    #[cfg(test)]
    pub(crate) fn transition_read_hook_status(
        &self,
    ) -> Option<(String, orbit_types::task::TaskStatus)> {
        self.transition_read_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
