//! [`OrbitRuntime`]: construction, identity, and the accessors command
//! handlers reach stores, policy, and settings through.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AuditEventInsertParams, AuditInvocationFields, V2AuditEventFilter, V2AuditEventRow,
};
use orbit_store::{Store, workspace_id_for_orbit_dir};
use orbit_types::record::{Audit, OrbitEvent};
use orbit_types::workflow::ShipMode;
use orbit_types::workspace::WorkspacePaths;
use serde_json::Value;

use super::config_path::validated_runtime_config_path;
use super::host_resource::{
    HostResourceMonitor, HostResourceProbe, HostResourceStatus, ResourceAdmission,
    default_host_resource_probe,
};
use super::host_signal::{
    FixedHostSignals, HostSignalProbe, ScheduledShutdown, default_host_signal_probe,
};
use super::task_pr_forge::{NoTaskPrForge, TaskPrForge, default_task_pr_forge};
use super::workspace::binding::WorkspaceRuntimeBinding;
use super::workspace::catalog;
use super::{builder, event_bus, worker_coordination};
use crate::context::{ActorIdentity, OrbitContext, OrbitStores};

/// One-shot callback for the next orphaned-leaf reconciliation on a runtime.
type OrphanReconcileCallback = Box<dyn FnOnce() + Send>;
type OrphanReconcileHook = Arc<Mutex<Option<OrphanReconcileCallback>>>;

#[derive(Clone)]
pub struct OrbitRuntime {
    pub(super) worker_invocation: Option<Arc<orbit_types::tool::WorkerInvocation>>,
    pub(super) owner_coordinator: Option<Arc<dyn orbit_tools::OwnerCoordinator>>,
    /// A follower drain's owner transport, supplied by the registry-owning
    /// composition layer that holds the federated destinations [ORB-13625].
    /// Absent on a standalone runtime, whose pull drain then refuses rather
    /// than reaching for a local store.
    pub(super) drain_owner_transport: Option<Arc<dyn orbit_tools::DrainOwnerTransport>>,
    pub(crate) context: OrbitContext,
    /// This process joined a live executable generation other than its own
    /// without recording itself, so it opened Orbit's state without write
    /// access. Reads work; operational state mutations must be skipped. Tool
    /// dispatch may append an audit row through a separate migration-free path.
    write_free: bool,
    workspace_binding: Option<Arc<WorkspaceRuntimeBinding>>,
    /// A higher-level registry may mark this local checkout as a replica. Core
    /// stays registry-neutral; it only carries the refusal supplied by that
    /// owner so every task-record writer shares one fail-closed gate.
    coordination_write_owner: Option<Arc<str>>,
    automation_machine_identity: Option<Arc<str>>,
    /// The operating system admission matches a task's `os:` tags against.
    /// This binary's own by default; a fixture composes another so one process
    /// can stand in for a host of each OS.
    host_os: Option<orbit_types::task::HostOs>,
    /// Supplied by the same registry-owning composition layer, for reads that
    /// span more than one workspace. Absent on a standalone runtime, which
    /// then answers only for its own checkout [ORB-11027].
    workspace_catalog: Option<Arc<dyn catalog::WorkspaceCatalog>>,
    /// Host lifecycle signals unattended admission consults before starting
    /// new work [ORB-12968]. The platform probe by default; tests inject one.
    host_signals: Arc<dyn HostSignalProbe>,
    /// Forge that closes a task's Orbit-authored PRs on its terminal
    /// decision. `gh` by default; tests inject a fake.
    task_pr_forge: Arc<dyn TaskPrForge>,
    host_resources: Arc<HostResourceMonitor>,
    pub event_log: event_bus::EventLog,
    /// Outcome of the [ORB-10012] workspace-layout pre-flight that ran when
    /// this runtime opened (empty `applied` when the layout was already
    /// current). Surfaced by `orbit migrate`.
    layout_report: Arc<orbit_store::workflow::layout::LayoutUpgradeReport>,
    _temp_dir: Option<Arc<builder::TempDir>>,
    /// One-shot hook run at the start of the next orphaned-leaf reconciliation.
    /// The pull-refill boundary test persists a graceful cancel there. Empty
    /// in production. Instance-scoped so concurrent tests cannot collide.
    orphan_reconcile_hook: OrphanReconcileHook,
    /// Test-only seam for approve/start/reject: after the locked `get_task`,
    /// mutate the named task so compare-and-set can observe a lost race.
    /// Instance-scoped so concurrent `cargo test` threads cannot collide on
    /// the deterministic first task ID.
    #[cfg(test)]
    transition_read_hook: Arc<Mutex<Option<(String, orbit_types::task::TaskStatus)>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbitRuntimeRoots {
    pub global_root: PathBuf,
    pub shared_root: PathBuf,
    pub local_root: PathBuf,
}

/// Whether the constructing process retains this runtime past a single command.
///
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLifetime {
    ShortLived,
    LongLived,
}

impl OrbitRuntime {
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
            automation_machine_identity: None,
            host_os: orbit_types::task::HostOs::current(),
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
            automation_machine_identity: None,
            host_os: orbit_types::task::HostOs::current(),
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

    pub fn with_actor(mut self, actor: ActorIdentity) -> Self {
        self.context.set_actor(actor);
        self
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

    /// Registry-owning composition supplies stable machine identity; Core never discovers it.
    pub fn with_automation_machine_identity(mut self, machine_id: Option<String>) -> Self {
        self.context
            .set_execution_location(machine_id.as_ref().map(|machine_id| {
                orbit_types::task::ExecutionLocation {
                    machine_id: machine_id.clone(),
                    machine_name: None,
                }
            }));
        self.automation_machine_identity = machine_id.map(Arc::from);
        self
    }

    pub fn automation_machine_identity(&self) -> Option<&str> {
        self.automation_machine_identity.as_deref()
    }

    /// Registered owner of the bound workspace, when composition supplied a
    /// binding that names one. Delivery automation resolves its default owner
    /// from this rather than from cwd or the running executor.
    pub(crate) fn workspace_owner_machine_id(&self) -> Option<&str> {
        self.workspace_binding
            .as_ref()
            .and_then(|binding| binding.owner_machine_id.as_deref())
    }

    /// Attach the declared remote owner for a replica checkout. This is set by
    /// the registry-owning composition layer, never inferred by Core.
    pub fn with_coordination_write_owner(mut self, owner_machine_id: Option<String>) -> Self {
        self.coordination_write_owner = owner_machine_id.map(Arc::from);
        self
    }

    /// Attach the workspace catalog that resolves federated search scope. Like
    /// the replica owner above, it is supplied by the registry-owning layer and
    /// never constructed by Core.
    pub fn with_workspace_catalog(mut self, catalog: Arc<dyn catalog::WorkspaceCatalog>) -> Self {
        self.workspace_catalog = Some(catalog);
        self
    }

    pub(crate) fn workspace_catalog(&self) -> Option<&Arc<dyn catalog::WorkspaceCatalog>> {
        self.workspace_catalog.as_ref()
    }

    /// Admit tasks as a host running `os` would (`None`: an OS outside the
    /// `os:` namespace, which runs only untagged tasks). A fixture composes
    /// two runtimes in one process and gives each the OS it stands in for.
    pub fn with_host_os(mut self, os: Option<orbit_types::task::HostOs>) -> Self {
        self.host_os = os;
        self
    }

    /// The operating system this runtime admits tasks for: what local
    /// admission checks a task's `os:` tags against, and what a pull drain
    /// declares to its owner.
    pub fn host_os(&self) -> Option<orbit_types::task::HostOs> {
        self.host_os
    }

    /// Replace the host-signal probe, so a fixture can present a scheduled
    /// shutdown without touching the real `/run` filesystem [ORB-12968].
    pub fn with_host_signal_probe(mut self, probe: Arc<dyn HostSignalProbe>) -> Self {
        self.host_signals = probe;
        self
    }

    /// Replace the forge a terminal task decision closes PRs through, so a
    /// fixture can observe closures without a real repository.
    pub fn with_task_pr_forge(mut self, forge: Arc<dyn TaskPrForge>) -> Self {
        self.task_pr_forge = forge;
        self
    }

    pub(crate) fn task_pr_forge(&self) -> &dyn TaskPrForge {
        self.task_pr_forge.as_ref()
    }

    /// The host shutdown or reboot currently pending, if any. While one is,
    /// unattended admission points start no new runs; in-flight runs are
    /// left alone.
    pub fn scheduled_host_shutdown(&self) -> Option<ScheduledShutdown> {
        self.host_signals.scheduled_shutdown()
    }

    /// Replace the resource probe for deterministic pressure/unknown fixtures.
    pub fn with_host_resource_probe(mut self, probe: Arc<dyn HostResourceProbe>) -> Self {
        self.host_resources = Arc::new(
            HostResourceMonitor::new(probe, self.context.settings().resource_throttle().clone())
                .with_shared_history(&self.global_root()),
        );
        self
    }

    /// Shared sampler/evaluator; fresh runtimes recover recent host history too.
    pub fn host_resource_monitor(&self) -> Arc<HostResourceMonitor> {
        Arc::clone(&self.host_resources)
    }

    /// Resource verdict for the host and filesystems used by this workspace.
    /// Admission consumers may consult it; this method changes no run state.
    pub fn host_resource_status(&self) -> HostResourceStatus {
        self.host_resources.snapshot(&self.host_resource_paths())
    }

    /// Whether host pressure holds new admissions now [ORB-13901]. Drains,
    /// pull drains and ship discovery start no new task while it throttles;
    /// running work is never touched. Unknown telemetry fails open.
    pub fn resource_admission(&self) -> ResourceAdmission {
        self.host_resources.admission(&self.host_resource_paths())
    }

    fn host_resource_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.paths().repo_root.clone(),
            self.shared_root().join("state/worktrees"),
            self.global_root(),
        ];
        paths.sort();
        paths.dedup();
        paths
    }

    /// Refuse control-plane work in a replica checkout.
    ///
    /// The refusal is a catalog-role capability outcome, not a malformed call:
    /// federated routing has to tell "this destination will not run that class"
    /// apart from "that request was invalid", so this reports
    /// `CapabilityRefused` [ORB-11012].
    pub(crate) fn ensure_coordination_task_write_permitted(&self) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            return Err(OrbitError::PolicyDenied(
                "claimed coordination writes require the owner route".into(),
            ));
        }
        let Some(owner_machine_id) = self.coordination_write_owner.as_deref() else {
            return Ok(());
        };
        Err(OrbitError::CapabilityRefused(format!(
            "control_plane coordination writes are refused in this replica checkout; workspace is owned by machine '{owner_machine_id}'"
        )))
    }

    pub(crate) fn coordination_task_reads_visible(&self) -> bool {
        self.coordination_write_owner.is_none()
    }

    /// The remote owner declared for a replica checkout, if this is one.
    pub(crate) fn coordination_write_owner(&self) -> Option<&str> {
        self.coordination_write_owner.as_deref()
    }

    /// Returns in-process events recorded during this session only. Not persisted across process
    /// boundaries — the log is empty at startup and discarded on exit. For the persistent CLI
    /// audit log written on every invocation, see [`OrbitRuntime::list_audit_events`].
    pub fn list_session_events(&self, limit: usize) -> Result<Vec<Audit>, OrbitError> {
        let events = self.event_log.snapshot();
        let audits = events
            .into_iter()
            .enumerate()
            .map(|(idx, event)| orbit_event_to_audit((idx + 1) as i64, event))
            .rev()
            .take(limit)
            .collect();
        Ok(audits)
    }

    pub fn shared_root(&self) -> PathBuf {
        self.context.shared_root().to_path_buf()
    }

    pub fn local_root(&self) -> PathBuf {
        self.context.local_root().to_path_buf()
    }

    pub fn data_root(&self) -> PathBuf {
        self.shared_root()
    }

    pub fn global_root(&self) -> PathBuf {
        self.context.global_root().to_path_buf()
    }

    /// Higher-level workspace metadata used to construct this runtime, when
    /// the caller supplied an authoritative binding.
    pub fn workspace_runtime_binding(&self) -> Option<&WorkspaceRuntimeBinding> {
        self.workspace_binding.as_deref()
    }

    /// Ship mode an unattended drain delivers in.
    ///
    /// The bound workspace's mode, or [`ShipMode::Local`] when this runtime
    /// has no workspace binding. Automatic admission, readiness and
    /// `orbit doctor` share this so they cannot disagree with the drain about
    /// a local-only route [ORB-14168].
    pub fn automatic_delivery_ship_mode(&self) -> ShipMode {
        self.workspace_runtime_binding()
            .map_or(ShipMode::Local, |binding| binding.ship_mode)
    }

    /// Short label naming this runtime's workspace in a refusal: the
    /// checkout directory's name, never an internal id.
    pub(crate) fn workspace_label(&self) -> String {
        let repo_root = self
            .workspace_binding
            .as_deref()
            .map_or(self.paths().repo_root.as_path(), |binding| {
                binding.repo_root.as_path()
            });
        repo_root.file_name().map_or_else(
            || repo_root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Returns the effective `config.toml` path.
    ///
    /// Workspace config replaces global if present; a genuinely missing
    /// workspace config falls back to global. Rejected workspace entries and
    /// inspection failures remain visible to the caller instead of silently
    /// changing precedence. This pathname records selection only; readers must
    /// still open the file through a race-safe boundary.
    pub fn config_path(&self) -> Result<PathBuf, OrbitError> {
        validated_runtime_config_path(self)
    }

    pub fn persistence_config_json(&self) -> Value {
        self.context.persistence().as_json_value()
    }

    pub fn automation_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::AutomationStoreBackend>, OrbitError> {
        Ok(Arc::clone(&self.context.stores().host.automation))
    }

    /// Before-PR review ledgers, certificates, and landings [ORB-11333].
    pub fn review_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::ReviewStoreBackend>, OrbitError> {
        Ok(Arc::clone(&self.context.stores().host.review))
    }

    pub fn sqlite_store(&self) -> Result<Store, OrbitError> {
        Ok(self.context.stores().host.sqlite.clone())
    }

    /// Persist dispatch telemetry without widening a foreign-generation
    /// reader's operational state access or running database migrations.
    pub(crate) fn record_tool_dispatch_audit(
        &self,
        params: &AuditEventInsertParams,
        invocation: AuditInvocationFields<'_>,
    ) -> Result<(), OrbitError> {
        if self.is_write_free() {
            Store::append_audit_event_at_path(
                &self.context.persistence().audit_db,
                params,
                invocation,
            )
        } else {
            self.context
                .stores()
                .host
                .sqlite
                .insert_audit_event_record_with_invocation(params, invocation)
        }
    }

    /// Probe the configured database path independently of cached runtime
    /// handles. Diagnostics must observe missing or replaced files and must
    /// not repair or migrate them as a side effect.
    pub fn sqlite_store_for_diagnostics(&self) -> Result<Store, OrbitError> {
        Store::open_read_only(&self.context.persistence().audit_db)
    }

    /// Check write readiness at the configured path without recreating or
    /// migrating the database, independently of cached runtime connections.
    pub fn check_sqlite_store_writable(&self) -> Result<(), OrbitError> {
        Store::check_path_writable(&self.context.persistence().audit_db)
    }

    pub fn v2_audit_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::V2AuditStoreBackend>, OrbitError> {
        Ok(Arc::clone(&self.context.stores().host.v2_audit))
    }

    pub fn ensure_persistence_ready(&self) -> Result<(), OrbitError> {
        orbit_store::compose::ensure_sqlite_store_ready(&self.context.persistence().audit_db)
    }

    pub fn workspace_id(&self) -> Result<String, OrbitError> {
        workspace_id_for_orbit_dir(&self.context.paths().orbit_dir)
    }

    pub fn list_v2_audit_events(
        &self,
        mut filter: V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        if filter.workspace_id.trim().is_empty() {
            filter.workspace_id = self.workspace_id()?;
        }
        self.v2_audit_store()?.list_v2_audit_events(&filter)
    }

    pub fn insert_v2_audit_event(
        &self,
        params: &orbit_store::contracts::V2AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        self.v2_audit_store()?.insert_v2_audit_event(params)
    }

    pub fn scoring_enabled(&self) -> bool {
        self.context.scoring_enabled()
    }

    /// Minutes a deferred delivery-automation reason may persist before the
    /// evaluator escalates it to a warning and one friction record.
    pub fn automation_stall_window_minutes(&self) -> u32 {
        self.context.settings().automation_stall_window_minutes()
    }

    pub fn pr_config(&self) -> &orbit_engine::PrConfig {
        self.context.settings().pr_config()
    }

    /// Config-only `[workflow] base_branch` fallback (default `"main"`).
    /// Delivery defaults must use [`Self::workspace_base_branch`], which
    /// prefers the registered workspace base branch when a binding exists.
    pub fn workflow_base_branch(&self) -> &str {
        self.context.settings().workflow_base_branch()
    }

    /// Commands every delivered candidate must pass on its exact commit
    /// (`[workflow] required_validation_commands`): the owner's own delivery
    /// runs them, and an owner accepts a distributed claim's handoff only with
    /// a passing log for each. Empty means no required check on any path: no
    /// command runs and a handoff carries no validation logs.
    pub fn workflow_required_validation_commands(&self) -> &[String] {
        self.context
            .settings()
            .workflow_required_validation_commands()
    }

    /// The note a drain submission shows when this host declares no required
    /// validation commands, so "no check" reads as a configured choice rather
    /// than a silent gap. `None` when commands are configured.
    pub fn required_validation_note(&self) -> Option<String> {
        self.workflow_required_validation_commands()
            .is_empty()
            .then(|| {
                "no `workflow.required_validation_commands` configured; delivered candidates \
                 run no required validation here"
                    .to_string()
            })
    }

    /// `[workflow.validation_env]` as admitted [ORB-13987].
    pub fn validation_env_policy(&self) -> &orbit_exec::ValidationEnvPolicy {
        self.context.settings().validation_env()
    }

    /// The environment owner-side repository tooling runs in: the allowlisted
    /// agent environment with PATH and toolchain locators resolved from the
    /// owner user's login shell (cached per process) and
    /// `workflow.validation_env.path`, never from whatever launched this
    /// process [ORB-13987].
    pub fn validation_environment(&self) -> orbit_exec::ValidationEnvironment {
        let base = self.execution_env_policy().agent_subprocess_env(&[]);
        let shell = orbit_exec::LoginShell::for_current_user(&base);
        orbit_exec::ValidationEnvironment::resolve(base, self.validation_env_policy(), &shell)
    }

    /// Why required validation may not find the user's toolchain on this
    /// host, for drain submission and `orbit doctor` [ORB-13987]. `None` when
    /// no required commands are configured or the environment resolved
    /// cleanly.
    pub fn validation_env_preflight_warning(&self) -> Option<String> {
        if self.workflow_required_validation_commands().is_empty() {
            return None;
        }
        self.validation_environment().preflight_warning()
    }

    /// `[workflow] distributed_completion`: `review` or `done`.
    pub fn workflow_distributed_completion(&self) -> &str {
        self.context.settings().workflow_distributed_completion()
    }

    /// `[workflow.task_pilot_freshness]`, before any routine override.
    pub(crate) fn task_pilot_freshness(
        &self,
    ) -> &orbit_types::workflow::automation::members::PreparationFreshness {
        self.context.settings().task_pilot_freshness()
    }

    /// The branch this workspace integrates into: the registered workspace
    /// base branch when a registry binding exists, else `[workflow]
    /// base_branch`. Delivery automation defaults are seeded against it.
    pub fn workspace_base_branch(&self) -> &str {
        self.workspace_runtime_binding()
            .and_then(|binding| binding.base_branch.as_deref())
            .unwrap_or_else(|| self.workflow_base_branch())
    }

    /// Whether this workspace opted into unattended ship dispatch
    /// (`[workflow] auto_ship` in the active `config.toml`; defaults to
    /// `false`). Consulted by `orbit run ship-sweep` and other schedulers
    /// before dispatching ship runs nobody explicitly asked for.
    pub fn workflow_auto_ship(&self) -> bool {
        self.context.settings().workflow_auto_ship()
    }

    /// The resolved `[operation]` review preferences (layered over the
    /// built-in no-review defaults) with per-field provenance [ORB-11333].
    pub fn operation_policy(&self) -> &orbit_config::OperationPolicy {
        self.context.settings().operation()
    }

    pub(crate) fn actor(&self) -> &ActorIdentity {
        self.context.actor()
    }

    pub(crate) fn actor_label(&self) -> &str {
        self.context.actor().label.as_str()
    }

    pub(crate) fn policy_engine(&self) -> &orbit_policy::PolicyEngine {
        self.context.policy()
    }

    pub(crate) fn tool_registry(&self) -> &orbit_tools::ToolRegistry {
        self.context.registry()
    }

    pub(crate) fn stores(&self) -> &OrbitStores {
        self.context.stores()
    }

    /// What the host plugin load pass registered and refused when this
    /// runtime was built.
    pub(crate) fn plugin_load(&self) -> &crate::runtime::plugin::host::PluginHostLoad {
        self.context.plugin_load()
    }

    pub(crate) fn skill_catalog(&self) -> &crate::skill_catalog::SkillCatalog {
        self.context.skill_catalog()
    }

    /// Resolved workspace paths. `pub` for the command surfaces extracted to
    /// `orbit-cmd` [ORB-10016].
    pub fn paths(&self) -> &WorkspacePaths {
        self.context.paths()
    }

    pub(crate) fn data_root_path(&self) -> &Path {
        self.shared_root_path()
    }

    pub(crate) fn shared_root_path(&self) -> &Path {
        self.context.shared_root()
    }

    pub(crate) fn execution_env_policy(&self) -> &orbit_config::ExecutionEnvPolicy {
        self.context.execution_env_policy()
    }

    pub(crate) fn codex_execution_policy(&self) -> &orbit_config::CodexExecutionPolicy {
        self.context.codex_execution_policy()
    }

    pub fn list_executor_defs(
        &self,
    ) -> Result<Vec<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().list_executor_defs()
    }

    pub fn get_executor_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().get_executor_def(name)
    }

    /// Where a dispatch from this workspace would launch an executor's
    /// `program` from, or `None` when it would fail to find it. Uses the same
    /// lookup as dispatch (`PATH`, conventional home bins, system prefixes).
    pub fn locate_provider_launcher(&self, program: &str) -> Option<std::path::PathBuf> {
        orbit_engine::activity_job::cli_runner::locate_provider_launcher(
            program,
            Some(&self.paths().repo_root),
        )
    }

    pub fn upsert_executor_def(
        &self,
        def: &orbit_types::workflow::ExecutorDef,
    ) -> Result<(), OrbitError> {
        self.stores().executors().upsert_executor_def(def)
    }

    pub fn list_policy_defs(&self) -> Result<Vec<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().list_policy_defs()
    }

    pub fn get_policy_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().get_policy_def(name)
    }

    pub fn upsert_policy_def(
        &self,
        def: &orbit_types::policy::PolicyDef,
    ) -> Result<(), OrbitError> {
        self.stores().policies().upsert_policy_def(def)
    }
}

fn orbit_event_to_audit(id: i64, event: OrbitEvent) -> Audit {
    let payload = serde_json::to_value(&event).unwrap_or(Value::Null);
    let event_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();

    Audit {
        id,
        event_type: event_type.clone(),
        payload,
        message: event_type,
        created_at: Utc::now(),
    }
}
