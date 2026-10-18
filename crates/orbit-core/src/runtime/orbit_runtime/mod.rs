//! [`OrbitRuntime`]: shared runtime types and method groups for construction,
//! identity, host probes, persistence, configuration, and internal accessors.

mod accessors;
mod admission_policy;
mod build;
mod config;
mod host;
mod identity;
mod persistence;
mod validation_warning;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::host_resource::HostResourceMonitor;
use super::host_resource::forecast::HostLoadProbe;
use super::host_resource::profile::ResourceProfileCache;
use super::host_signal::HostSignalProbe;
use super::task_pr_forge::TaskPrForge;
use super::workspace::binding::WorkspaceRuntimeBinding;
use super::workspace::catalog;
use super::{builder, event_bus};
use crate::context::OrbitContext;

/// Configuration reload supplied by the composition root. Runtime policy
/// refreshes may call it after config files change, but runtime never opens
/// config layers itself.
pub(crate) type RuntimeConfigLoader =
    Arc<dyn Fn() -> Result<orbit_config::ResolvedConfig, orbit_common::OrbitError> + Send + Sync>;

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
    automation_execution_location: Option<Arc<orbit_types::task::ExecutionLocation>>,
    /// The operating system admission matches a task's `os:` tags against.
    /// This binary's own by default; a fixture composes another so one process
    /// can stand in for a host of each OS.
    host_os: Option<orbit_types::task::HostOs>,
    /// The home whose headless browser kit admission reads for `ui` tasks.
    /// `None` is the current user's, read again on every admission pass; a
    /// fixture points it at a home it controls.
    browser_kit_home: Option<std::path::PathBuf>,
    /// One shared base-run budget per owner claim settlement. Leaves five
    /// minutes of the routed call's fifteen for observation, cleanup and reply.
    pub(crate) claim_known_red_budget: std::time::Duration,
    pub(crate) ci_failure_time: Option<chrono::DateTime<chrono::Utc>>,
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
    /// The host's resource profiles, shared by clones and kept across
    /// admission passes until a delivery finishes.
    resource_profiles: Arc<ResourceProfileCache>,
    admission_policy: Arc<Mutex<Option<admission_policy::CachedPolicy>>>,
    admission_policy_snapshot: Option<Arc<admission_policy::AdmissionPolicy>>,
    config_loader: RuntimeConfigLoader,
    /// The host's measured load for the forecast baseline. It keeps only the
    /// previous CPU tick reading that utilization is measured against.
    host_load: Arc<HostLoadProbe>,
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
