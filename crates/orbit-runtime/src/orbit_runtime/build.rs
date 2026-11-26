//! Runtime construction and deterministic reconciliation hooks.

use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_types::workflow::ShipMode;

use super::{HostLifetime, OrbitRuntimeRoots, RuntimeConfigLoader, RuntimeKernel};
use crate::application_port::ApplicationPort;
use crate::base::Base;
use crate::base::binding::WorkspaceRuntimeBinding;
use crate::host::Host;
use crate::host::context::OrbitContext;
use crate::host_resource::{HostResourceMonitor, default_host_resource_probe};
use crate::host_signal::{FixedHostSignals, HostSignalProbe, default_host_signal_probe};
use crate::run::Run;
use crate::stored::Stored;
use crate::task_pr_forge::{NoTaskPrForge, TaskPrForge, default_task_pr_forge};
use crate::{builder, worker_coordination};

impl RuntimeKernel {
    /// Inject a caller's environment snapshot into a test fixture.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_env(mut self, env: orbit_common::Env) -> Self {
        let base = self.base_mut();
        base.policy.execution_env_policy = base
            .policy
            .execution_env_policy
            .clone()
            .with_parent_env(&env);
        self
    }

    /// Set the observation clock for deterministic CI-failure filing.
    /// Without an override, filing uses the current UTC time.
    pub fn with_ci_failure_time(mut self, now: chrono::DateTime<chrono::Utc>) -> Self {
        self.base_mut().ci_failure_time = Some(now);
        self
    }

    /// Only the accepting transport's authenticated facts may label artifact bytes.
    pub fn artifact_origin(
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
    pub fn build_from_resolved_config(
        roots: &OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
        runtime_config: &orbit_config::ResolvedConfig,
        layout_report: orbit_store::migrate::layout::LayoutUpgradeReport,
        host_lifetime: HostLifetime,
        read_only: bool,
        config_loader: RuntimeConfigLoader,
        application: &'static dyn ApplicationPort,
        task_prefix: Option<&str>,
        store: orbit_store::Store,
    ) -> Result<Self, OrbitError> {
        let access = if read_only {
            builder::StateAccess::ReadOnly
        } else {
            builder::StateAccess::Write
        };
        Self::finish_from_context(
            builder::build_context_from_roots(
                roots,
                binding.as_ref(),
                runtime_config,
                host_lifetime,
                access,
                task_prefix,
                store,
            )?,
            binding,
            &roots.global_root,
            layout_report,
            config_loader,
            application,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_from_resolved_config_write_free(
        roots: &OrbitRuntimeRoots,
        binding: Option<WorkspaceRuntimeBinding>,
        runtime_config: &orbit_config::ResolvedConfig,
        layout_report: orbit_store::migrate::layout::LayoutUpgradeReport,
        host_lifetime: HostLifetime,
        config_loader: RuntimeConfigLoader,
        application: &'static dyn ApplicationPort,
        store: orbit_store::Store,
    ) -> Result<Self, OrbitError> {
        let mut runtime = Self::finish_from_context(
            builder::build_context_from_roots(
                roots,
                binding.as_ref(),
                runtime_config,
                host_lifetime,
                builder::StateAccess::WriteFree,
                None,
                store,
            )?,
            binding,
            &roots.global_root,
            layout_report,
            config_loader,
            application,
        )?;
        runtime.stored_mut().write_free = true;
        Ok(runtime)
    }

    /// Whether this runtime opened Orbit's state without write access, for
    /// observational routine status or a live generation it cannot record into.
    /// Reads work; operations that would repair or finalize stored state must
    /// be skipped rather than attempted. Tool audit appends use a separate
    /// connection without granting writes to these runtime state handles.
    pub fn is_write_free(&self) -> bool {
        self.stored().write_free
    }

    fn finish_from_context(
        context: OrbitContext,
        binding: Option<WorkspaceRuntimeBinding>,
        global_root: &Path,
        layout_report: orbit_store::migrate::layout::LayoutUpgradeReport,
        config_loader: RuntimeConfigLoader,
        application: &'static dyn ApplicationPort,
    ) -> Result<Self, OrbitError> {
        let host_resources = Arc::new(
            HostResourceMonitor::new(
                default_host_resource_probe(),
                context.settings.resource_throttle().clone(),
            )
            .with_shared_history(global_root)
            .sampled_in_background(),
        );
        let env = context.policy.execution_env_policy.process_env();
        let worker_invocation = worker_coordination::restore_process_binding(env, global_root)?;
        let task_pr_forge = default_task_pr_forge(env);
        Ok(Self::from_layers(
            application,
            context,
            config_loader,
            binding.map(Arc::new),
            None,
            layout_report,
            host_resources,
            default_host_signal_probe(),
            task_pr_forge,
            None,
            worker_invocation,
        ))
    }

    /// Sort one open's parts into the four layers, bottom up.
    #[allow(clippy::too_many_arguments)]
    fn from_layers(
        application: &'static dyn ApplicationPort,
        context: OrbitContext,
        config_loader: RuntimeConfigLoader,
        binding: Option<Arc<WorkspaceRuntimeBinding>>,
        temp_dir: Option<Arc<builder::TempDir>>,
        layout_report: orbit_store::migrate::layout::LayoutUpgradeReport,
        host_resources: Arc<HostResourceMonitor>,
        host_signals: Arc<dyn HostSignalProbe>,
        task_pr_forge: Arc<dyn TaskPrForge>,
        browser_kit_home: Option<std::path::PathBuf>,
        worker_invocation: Option<Arc<orbit_types::tool::WorkerInvocation>>,
    ) -> Self {
        let base = Base::new(
            context.paths,
            context.policy,
            context.settings,
            config_loader,
            binding,
            temp_dir,
        );
        let stored = Stored::new(base, context.stores, layout_report);
        let host = Host::new(
            stored,
            context.execution,
            host_resources,
            host_signals,
            task_pr_forge,
            browser_kit_home,
        );
        Self {
            application,
            run: Run::new(host, worker_invocation),
            orphan_reconcile_hook: Arc::new(Mutex::new(None)),
            #[cfg(any(test, feature = "test-support"))]
            transition_read_hook: Arc::new(Mutex::new(None)),
            #[cfg(any(test, feature = "test-support"))]
            transition_run_hook: Arc::new(Mutex::new(None)),
        }
    }

    pub fn build_in_memory_from_resolved_config(
        data_root: &Path,
        workspace_root: &Path,
        runtime_config: &orbit_config::ResolvedConfig,
        temp_dir: builder::TempDir,
        config_loader: RuntimeConfigLoader,
        application: &'static dyn ApplicationPort,
        store: orbit_store::Store,
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
        let roots = OrbitRuntimeRoots {
            global_root: data_root.to_path_buf(),
            shared_root: workspace_root.to_path_buf(),
            local_root: workspace_root.to_path_buf(),
        };
        let context = builder::build_context_from_roots(
            &roots,
            Some(&binding),
            runtime_config,
            HostLifetime::ShortLived,
            builder::StateAccess::Write,
            None,
            store,
        )?;
        let host_resources = Arc::new(
            HostResourceMonitor::new(
                default_host_resource_probe(),
                context.settings.resource_throttle().clone(),
            )
            .with_shared_history(data_root)
            .sampled_in_background(),
        );
        Ok(Self::from_layers(
            application,
            context,
            config_loader,
            Some(Arc::new(binding)),
            Some(Arc::new(temp_dir)),
            orbit_store::migrate::layout::LayoutUpgradeReport::default(),
            host_resources,
            // An in-memory runtime is not bound to a host lifecycle.
            Arc::new(FixedHostSignals::none()),
            // Nor is it bound to a repository whose PRs it could close.
            Arc::new(NoTaskPrForge),
            // An in-memory runtime reads no user's browser kit: its data root
            // holds none.
            Some(data_root.to_path_buf()),
            None,
        ))
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

    pub fn run_orphan_reconcile_hook(&self) {
        let hook = self
            .orphan_reconcile_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(hook) = hook {
            hook();
        }
    }

    /// Shorten the owner settlement budget in deterministic boundary fixtures.
    #[cfg(feature = "test-support")]
    pub fn with_claim_known_red_budget(mut self, budget: std::time::Duration) -> Self {
        self.host_mut().claim_known_red_budget = budget;
        self
    }

    /// Outcome of the workspace-layout pre-flight that ran when this runtime
    /// opened: which layout migrations (if any) were auto-applied.
    pub fn layout_upgrade_report(&self) -> &orbit_store::migrate::layout::LayoutUpgradeReport {
        &self.stored().layout_report
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn set_transition_read_hook_status(
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn transition_read_hook_status(&self) -> Option<(String, orbit_types::task::TaskStatus)> {
        self.transition_read_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Arm the dashboard-archive race seam for `id` (`None` disarms it).
    /// While armed, the archive path inserts a non-terminal local run owning
    /// `id` after its outer ownership pre-check, so the under-lock recheck
    /// observes the lost race. Production never arms it.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_transition_run_hook_task(&self, id: Option<&str>) {
        *self
            .transition_run_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = id.map(str::to_string);
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn transition_run_hook_task(&self) -> Option<String> {
        self.transition_run_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
