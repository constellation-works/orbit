//! Runtime bootstrap and the two-root architecture (global + workspace).
//!
//! `OrbitRuntime` is initialized by locating two roots:
//! 1. **Global root** — `~/.orbit/`: houses global config,
//!    the audit SQLite database, skills, and globally-scoped resources.
//! 2. **Workspace root** — the nearest ancestor `.orbit/` directory from cwd:
//!    houses workspace-local tasks, knowledge, optional skill overrides, and runtime state.
//!
//! The `resolve` sub-module implements root discovery. The `builder` sub-module
//! wires together stores, policy, tool registry, and event bus into a complete
//! [`OrbitRuntime`]. The `engine`, `audit`, `mutation`, and `tool_exec` sub-modules
//! provide the high-level operations exposed to command handlers.

pub(crate) mod assets;
pub mod audit;
mod authorization;
pub mod builder;
pub(crate) mod command_exec;
mod coordination_audit;
pub(crate) mod cwd;
pub mod engine;
pub mod event_bus;
pub(crate) mod friction;
#[cfg(target_os = "linux")]
pub(crate) mod git_sandbox;
pub mod mutation;
pub(crate) mod recovery_authority;
mod resolve;
pub mod run_audit;
pub(crate) mod run_input;
pub(crate) mod task;
pub use task::StaleTaskReservation;
pub(crate) mod tool_exec;
pub mod workspace_catalog;
pub mod workspace_claim;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::activity_job::{CatalogDirectory, CatalogDirectoryList};
use orbit_store::contracts::{V2AuditEventFilter, V2AuditEventRow};
use orbit_store::{Store, workspace_id_for_orbit_dir};
use orbit_types::record::{Audit, OrbitEvent};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspacePaths};
use serde_json::Value;

use crate::context::ActorIdentity;
use crate::context::OrbitContext;
use crate::context::OrbitStores;
use crate::runtime::assets::DEFAULT_ACTIVITY_FILES;
use orbit_types::workflow::{ShipMode, resolved_ship_mode};

pub(crate) use resolve::{resolve_bootstrap_roots, resolve_initialize_roots};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use resolve::{
    ResolvedOrbitRoots, WorkspaceRootHint, resolve_bootstrap_roots_with_hint,
    resolve_initialize_roots_with_hint, try_resolve_initialized_roots_with_hint,
};
// `pub` for the runtime-less `orbit migrate --dry-run` inspection that moved
// to `orbit-cmd` [ORB-10016].
pub use resolve::{is_global_orbit_root, resolve_global_root, try_resolve_initialized_roots};
// `pub` for host task-store maintenance in `orbit-cmd`, which must recognize
// the one partition id no registry claims [ORB-12119].
pub use builder::UNBOUND_DATA_DIR_PARTITION_ID;
pub use run_input::managed_workspace_selector_from_env;
pub(crate) use task::{failed_run_error_context, is_workflow_failure_state};

#[cfg(test)]
pub(crate) type AfterLockedStateReadHook = Arc<dyn Fn(&orbit_types::task::Task) + Send + Sync>;

#[derive(Clone)]
pub struct OrbitRuntime {
    pub(crate) context: OrbitContext,
    workspace_binding: Option<Arc<WorkspaceRuntimeBinding>>,
    /// A higher-level registry may mark this local checkout as a replica. Core
    /// stays registry-neutral; it only carries the refusal supplied by that
    /// owner so every task-record writer shares one fail-closed gate.
    coordination_write_owner: Option<Arc<str>>,
    automation_machine_identity: Option<Arc<str>>,
    /// Supplied by the same registry-owning composition layer, for reads that
    /// span more than one workspace. Absent on a standalone runtime, which
    /// then answers only for its own checkout [ORB-11027].
    workspace_catalog: Option<Arc<dyn workspace_catalog::WorkspaceCatalog>>,
    pub event_log: event_bus::EventLog,
    /// Outcome of the [ORB-10012] workspace-layout pre-flight that ran when
    /// this runtime opened (empty `applied` when the layout was already
    /// current). Surfaced by `orbit migrate`.
    layout_report: Arc<orbit_store::workflow::layout::LayoutUpgradeReport>,
    _temp_dir: Option<Arc<builder::TempDir>>,
    /// Test-only seam for `apply_task_automation_update`: fired after the
    /// authoritative locked `get_task`, before the derived write. The lock is
    /// re-entrant, so a hook may mutate the same task and observe whether the
    /// automation write merges or refuses.
    #[cfg(test)]
    after_locked_state_read: Arc<Mutex<Option<AfterLockedStateReadHook>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrbitRuntimeRoots {
    pub global_root: PathBuf,
    pub shared_root: PathBuf,
    pub local_root: PathBuf,
}

/// Whether the constructing process retains this runtime past a single command.
///
/// Short-lived CLI mutations must not spawn a detached embed worker: process
/// exit can interrupt queued indexing, and a missing companion must not add
/// startup or retry work. Long-lived hosts (MCP serve, the dashboard) keep
/// incremental indexing on [`orbit_search::EmbedWorker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLifetime {
    ShortLived,
    LongLived,
}

/// Registry-neutral metadata supplied by a higher-level workspace catalog.
///
/// `orbit-core` can construct a runtime without this binding for standalone
/// compatibility. Multi-host composition supplies it explicitly so runtime
/// path and ship-mode decisions do not have to reopen a registry owned by a
/// higher feature crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRuntimeBinding {
    /// Logical catalog ID (`ws_*`). Nested managed CLI/MCP calls use this
    /// selector instead of rediscovering ownership from a linked-worktree cwd.
    pub logical_workspace_id: String,
    /// Task-store partition id: the checkout identity from
    /// `.orbit/config.yaml` that the task registry partitions task bundles by
    /// (`workspace_bindings.workspace_id`). A different namespace from
    /// `logical_workspace_id` above, which the workspace registry mints, and
    /// the two genuinely differ for any checkout bound before `workspace init`
    /// supplied an id (L-0098).
    pub task_partition_id: String,
    /// Registered owner of the logical workspace. Automation resolves the
    /// default owner of a delivery definition from it, so an unambiguously
    /// owned workspace needs no redundant per-definition configuration.
    /// Absent on standalone registries that predate host identity.
    pub owner_machine_id: Option<String>,
    pub repo_root: PathBuf,
    pub ship_mode: ShipMode,
    /// Registered integration branch; absent for standalone runtimes.
    pub base_branch: Option<String>,
}

/// Build the neutral Core binding for one registered local checkout.
pub fn workspace_runtime_binding(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
) -> Result<WorkspaceRuntimeBinding, OrbitError> {
    Ok(WorkspaceRuntimeBinding {
        logical_workspace_id: workspace.id.clone(),
        task_partition_id: workspace_id_for_orbit_dir(&checkout.orbit_dir)?,
        owner_machine_id: workspace.owner_machine_id.clone(),
        repo_root: checkout.repo_root.clone(),
        ship_mode: resolved_ship_mode(workspace),
        base_branch: Some(workspace.base_branch.clone()),
    })
}

impl OrbitRuntime {
    pub(crate) fn build_from_resolved_config(
        global_root: &Path,
        shared_root: &Path,
        local_root: &Path,
        binding: Option<WorkspaceRuntimeBinding>,
        runtime_config: &orbit_config::ResolvedConfig,
        layout_report: orbit_store::workflow::layout::LayoutUpgradeReport,
        host_lifetime: HostLifetime,
    ) -> Result<Self, OrbitError> {
        let context = builder::build_context_from_roots(
            global_root,
            shared_root,
            local_root,
            binding.as_ref(),
            runtime_config,
            host_lifetime,
        )?;
        Ok(Self {
            context,
            workspace_binding: binding.map(Arc::new),
            coordination_write_owner: None,
            automation_machine_identity: None,
            workspace_catalog: None,
            event_log: event_bus::EventLog::default(),
            layout_report: Arc::new(layout_report),
            _temp_dir: None,
            #[cfg(test)]
            after_locked_state_read: Arc::new(Mutex::new(None)),
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
        )?;
        Ok(Self {
            context,
            workspace_binding: Some(Arc::new(binding)),
            coordination_write_owner: None,
            automation_machine_identity: None,
            workspace_catalog: None,
            event_log: event_bus::EventLog::default(),
            layout_report: Arc::new(orbit_store::workflow::layout::LayoutUpgradeReport::default()),
            _temp_dir: Some(Arc::new(temp_dir)),
            #[cfg(test)]
            after_locked_state_read: Arc::new(Mutex::new(None)),
        })
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
    pub(crate) fn set_after_locked_state_read_hook(&self, hook: AfterLockedStateReadHook) {
        *self
            .after_locked_state_read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    pub(crate) fn invoke_after_locked_state_read(&self, task: &orbit_types::task::Task) {
        let hook = self
            .after_locked_state_read
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(task);
        }
    }

    /// Registry-owning composition supplies stable machine identity; Core never discovers it.
    pub fn with_automation_machine_identity(mut self, machine_id: Option<String>) -> Self {
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
    pub fn with_workspace_catalog(
        mut self,
        catalog: Arc<dyn workspace_catalog::WorkspaceCatalog>,
    ) -> Self {
        self.workspace_catalog = Some(catalog);
        self
    }

    pub(crate) fn workspace_catalog(
        &self,
    ) -> Option<&Arc<dyn workspace_catalog::WorkspaceCatalog>> {
        self.workspace_catalog.as_ref()
    }

    /// Refuse control-plane work in a replica checkout.
    ///
    /// The refusal is a catalog-role capability outcome, not a malformed call:
    /// federated routing has to tell "this destination will not run that class"
    /// apart from "that request was invalid", so this reports
    /// `CapabilityRefused` [ORB-11012].
    pub(crate) fn ensure_coordination_task_write_permitted(&self) -> Result<(), OrbitError> {
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

    /// Test seam: restate the registered workspace owner that registry
    /// composition supplies in production, so ownership resolution can be
    /// exercised on an in-memory runtime.
    #[cfg(test)]
    pub(crate) fn with_workspace_owner_machine_id(mut self, owner: Option<&str>) -> Self {
        if let Some(binding) = self.workspace_binding.as_ref() {
            let mut rebound = (**binding).clone();
            rebound.owner_machine_id = owner.map(ToOwned::to_owned);
            self.workspace_binding = Some(Arc::new(rebound));
        }
        self
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

    /// Operation-mode grants and recovery ledgers in the host store [ORB-11332].
    pub fn operation_store(
        &self,
    ) -> Result<Arc<dyn orbit_store::contracts::OperationStoreBackend>, OrbitError> {
        Ok(Arc::clone(&self.context.stores().host.operation))
    }

    pub fn sqlite_store(&self) -> Result<Store, OrbitError> {
        Ok(self.context.stores().host.sqlite.clone())
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

    /// Default base branch for ship workflows. Sourced
    /// from `[workflow] base_branch` in the active `config.toml`; defaults
    /// to `"main"` when no key is present.
    pub fn workflow_base_branch(&self) -> &str {
        self.context.settings().workflow_base_branch()
    }

    /// Whether this workspace opted into unattended ship dispatch
    /// (`[workflow] auto_ship` in the active `config.toml`; defaults to
    /// `false`). Consulted by `orbit run ship-sweep` and other schedulers
    /// before dispatching ship runs nobody explicitly asked for.
    pub fn workflow_auto_ship(&self) -> bool {
        self.context.settings().workflow_auto_ship()
    }

    /// The resolved operation-mode preferences (`[operation]` layered over
    /// the built-in supervised defaults) with per-field provenance. These are
    /// preferences: they authorize nothing by themselves [ORB-11332].
    pub fn operation_policy(&self) -> &orbit_config::OperationPolicy {
        self.context.settings().operation()
    }

    /// Build the activity catalog for `target: activity:<name>` resolution
    /// (Phase 4). Execution keeps shipped global activities authoritative:
    /// workspace-local assets can add new names, but cannot shadow binary
    /// defaults.
    ///
    /// The lookup order:
    /// 1. `ORBIT_ACTIVITY_DIR` env var (or legacy `ORBIT_V2_CATALOG_DIR`) as
    ///    a colon-separated list of dirs, highest precedence for smokes/tests.
    /// 2. `<global_root>/resources/activities/` — global defaults (seeded by
    ///    `orbit init` from the YAMLs embedded in the binary).
    /// 3. `<workspace_root>/.orbit/resources/activities/` — workspace-local
    ///    additions. Names matching shipped defaults are ignored unless an
    ///    explicit env catalog already supplied that name.
    ///
    /// Missing directories are skipped silently. Directories are loaded from
    /// highest to lowest precedence; the first activity for each name wins,
    /// and workspace-local shipped names are skipped even when the global file
    /// is missing.
    /// Duplicate names inside one directory tree are still a hard error
    /// (`CatalogError::DuplicateName`).
    pub fn v2_activity_catalog(
        &self,
    ) -> Result<
        orbit_engine::activity_job::V2ActivityCatalog,
        orbit_engine::activity_job::CatalogError,
    > {
        use orbit_engine::activity_job::V2ActivityCatalog;

        let mut catalog = V2ActivityCatalog::new();
        for dir in self.v2_activity_catalog_dirs() {
            if !dir.path().is_dir() {
                continue;
            }
            // L-0060 / ORB-00356: name-based execution keeps shipped defaults
            // authoritative over workspace catalogs.
            match dir.kind() {
                V2ActivityCatalogDirKind::Explicit | V2ActivityCatalogDirKind::Global => {
                    warn_skipped_retired_activity_assets(
                        dir.path(),
                        catalog.load_dir_skipping_retired_prefer_existing(dir.path())?,
                    );
                }
                V2ActivityCatalogDirKind::WorkspaceLocal => {
                    warn_skipped_retired_activity_assets(
                        dir.path(),
                        catalog.load_dir_skipping_retired_prefer_existing_where(
                            dir.path(),
                            |name| !is_default_activity_name(name),
                        )?,
                    );
                }
            }
        }
        let registered_tools = self.allowlist_known_tool_names();
        catalog.validate_tool_allowlists(registered_tools.iter().map(String::as_str))?;

        Ok(catalog)
    }

    /// Tool names valid as activity allowlist targets.
    ///
    /// This is the registry's builtin schema set.
    /// `pub` for the direct v2 activity runner in `orbit-cmd` [ORB-10016].
    pub fn allowlist_known_tool_names(&self) -> Vec<String> {
        self.tool_registry()
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect()
    }

    /// Production activity-catalog directories in load order. Doctor reuses
    /// this list so a workspace file that fails catalog construction cannot
    /// be reported healthy.
    pub(crate) fn v2_activity_catalog_paths(&self) -> Vec<PathBuf> {
        self.v2_activity_catalog_dirs()
            .into_iter()
            .map(|dir| dir.path().to_path_buf())
            .collect()
    }

    fn v2_activity_catalog_dirs(&self) -> Vec<CatalogDirectory<V2ActivityCatalogDirKind>> {
        let mut dirs = CatalogDirectoryList::default();

        let env_dirs = std::env::var("ORBIT_ACTIVITY_DIR")
            .ok()
            .or_else(|| std::env::var("ORBIT_V2_CATALOG_DIR").ok());
        if let Some(raw) = env_dirs {
            for entry in raw.split(':').filter(|value| !value.is_empty()) {
                dirs.push(
                    std::path::PathBuf::from(entry),
                    V2ActivityCatalogDirKind::Explicit,
                );
            }
        }

        dirs.push(
            self.context.paths().global_dir.join("resources/activities"),
            V2ActivityCatalogDirKind::Global,
        );
        dirs.push(
            self.context.paths().activities_dir.clone(),
            V2ActivityCatalogDirKind::WorkspaceLocal,
        );
        dirs.into_vec()
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

/// Select the fixed config leaf from roots owned by an initialized runtime.
///
/// Keeping this as a free-function boundary gives static analysis a precise
/// callable whose return is authoritative. It does not authorize an arbitrary
/// caller-supplied root: both roots come from the runtime's resolved context.
fn validated_runtime_config_path(runtime: &OrbitRuntime) -> Result<PathBuf, OrbitError> {
    let shared_root = runtime.shared_root();
    let global_root = runtime.global_root();
    if shared_root != global_root
        && let Some(workspace_config) = existing_workspace_config_path(&shared_root)?
    {
        return Ok(workspace_config);
    }

    Ok(global_root.join(CONFIG_TOML_FILE))
}

const CONFIG_TOML_FILE: &str = "config.toml";

/// Resolve an existing config root to the directory selected by the caller.
///
/// Runtime overrides and trusted directory aliases remain supported: an
/// existing symlinked root resolves to its canonical target. Returning that
/// validated root before deriving the fixed filename puts the validation
/// boundary ahead of the child metadata probe. Final-component safety belongs
/// to the descriptor-based reader; this boundary does not claim protection
/// against privileged replacement of mutable ancestors.
fn validated_existing_config_root(root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let canonical_root = match root.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to canonicalize config root '{}': {error}",
                root.display()
            )));
        }
    };

    Ok(Some(canonical_root))
}

/// Select the fixed workspace config leaf after the root validation boundary.
///
/// This no-follow metadata probe decides precedence and rejects an already
/// visible invalid leaf. It does not authorize a later pathname read; config
/// consumers reopen the selected path through their descriptor-based boundary.
fn existing_workspace_config_path(root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let Some(validated_root) = validated_existing_config_root(root)? else {
        return Ok(None);
    };
    let candidate = validated_root.join(CONFIG_TOML_FILE);

    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(OrbitError::InvalidInput(format!(
                "config path must be a regular {CONFIG_TOML_FILE} file inside '{}': {}",
                root.display(),
                candidate.display()
            )))
        }
        Ok(_) => Ok(Some(candidate)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect config path '{}': {error}",
            candidate.display()
        ))),
    }
}

fn warn_skipped_retired_activity_assets(dir: &Path, skipped: Vec<PathBuf>) {
    if skipped.is_empty() {
        return;
    }
    tracing::warn!(
        target: "orbit.core.assets",
        count = skipped.len(),
        dir = %dir.display(),
        "skipped retired schemaVersion 1 activity assets while loading",
    );
}

#[derive(Clone, Copy)]
enum V2ActivityCatalogDirKind {
    Explicit,
    Global,
    WorkspaceLocal,
}

fn is_default_activity_name(name: &str) -> bool {
    DEFAULT_ACTIVITY_FILES
        .iter()
        .any(|(default_name, _)| *default_name == name)
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
