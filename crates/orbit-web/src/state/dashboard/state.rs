use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use orbit_cmd::registry_runtime::ConfigLayerStamp;
use orbit_common::Env;
use orbit_core::app::run;
use orbit_core::application::routines::ClockStatus;
use orbit_core::runtime::WorkspaceRuntimeBinding;
use orbit_core::{Core, OrbitError, ShipMode};

#[cfg(test)]
use super::super::PrePublishHook;
use super::super::{
    CheckoutFingerprint, HostFileState, INITIAL_GENERATION, Pinned, PinnedHosts,
    RegistryFingerprint, RegistrySource, SINGLE_WORKSPACE_ID, Snapshot, SnapshotData, WsEntry,
    WsRejection, checkout_fingerprints,
};
use super::super::{HostTunnels, TunnelConfig, run_read};
use super::cache::CachedRuntime;
use crate::doctor_report::DoctorReports;
use crate::runtime_memo::RuntimeMemo;

/// Test override for native clock status reads.
#[cfg(test)]
pub(crate) type ClockStatusHook = Arc<dyn Fn() -> Result<ClockStatus, OrbitError> + Send + Sync>;

/// Test override for a successful native clock mutation.
#[cfg(test)]
pub(crate) type ClockMutationHook = Arc<dyn Fn() -> Result<(), OrbitError> + Send + Sync>;

pub(in crate::state) struct StateInner {
    /// Captured process environment handed in at server startup.
    env: Env,
    /// The served Orbit root: an explicit `--root`, else `~/.orbit`. Passed as
    /// `global_root` when building per-workspace runtimes and observing host resources.
    pub(super) global_root: PathBuf,
    host_resources: Mutex<Option<Arc<orbit_core::runtime::host_resource::HostResourceMonitor>>>,
    host_disk_paths: Vec<PathBuf>,
    /// Atomically-swapped registered workspace set + default selection.
    pub(super) snapshot: Mutex<Arc<Snapshot>>,
    /// The serving host's host file, swapped by the same rule [ORB-14451].
    hosts: HostFileState,
    /// SSH tunnels to registered hosts behind `/api/on/<host>/…` [ORB-14679].
    host_tunnels: Arc<HostTunnels>,
    /// Lazily-built, cached runtimes keyed by workspace id.
    pub(super) runtimes: Mutex<HashMap<String, CachedRuntime>>,
    /// Registry to reload from on refresh; `None` disables refresh (single /
    /// direct-entry global modes).
    pub(super) source: Option<RegistrySource>,
    /// Serializes refreshes so a snapshot swap and its runtime eviction are one
    /// atomic step relative to other refreshes. Never held across runtime
    /// construction. The request-path fast path (`pin` with unchanged registry
    /// and checkout fingerprints) does not take this lock.
    pub(super) refresh_lock: Mutex<()>,
    /// `workspaces.json` mtime+len sampled before the last successful load.
    /// Compared on `pin` without `refresh_lock`; updated only after a
    /// successful snapshot swap.
    pub(super) last_fingerprint: Mutex<Option<RegistryFingerprint>>,
    /// Last successfully observed filesystem state for the registered
    /// checkouts in the current snapshot. Compared on `pin` without
    /// `refresh_lock` so disappeared and repaired checkouts are revalidated
    /// without rereading a steady-state registry.
    pub(super) last_checkout_fingerprints: Mutex<Vec<CheckoutFingerprint>>,
    /// Allocates strictly-increasing generations for published snapshots.
    pub(super) generation_counter: AtomicU64,
    /// Per-server memo for `/api/audit/summary`. Keyed by runtime identity
    /// and the raw `since` window so relative cutoffs (`24h`) still hit.
    audit_summary: RuntimeMemo<String>,
    /// Per-server Cost page memo, keyed by live workspace runtime and window.
    cost: RuntimeMemo<String>,
    /// Host-wide routine projection, invalidated by its input stamps.
    routines: RuntimeMemo<crate::runtime_memo::RoutineMemoKey>,
    /// Per-server single-flight TTL memo for audited plugin panel reads.
    /// Keyed by `(namespace, panel)`.
    plugin_panels: RuntimeMemo<(String, String)>,
    /// Per-server memo for `/api/diagnostics/errors`, keyed by time range and
    /// bounded row limit. Collapses overlapping Errors-tab polls into one scan.
    diagnostics_errors: RuntimeMemo<(String, usize)>,
    /// Per-server memo for `/api/diagnostics/friction`, keyed by month and
    /// bounded row limit. Collapses overlapping Runs-tab polls into one scan.
    diagnostics_friction: RuntimeMemo<(String, usize)>,
    /// Per-server memo for the Analytics › Metrics trends, keyed by endpoint
    /// and normalized query string. Collapses overlapping Metrics-tab polls
    /// (implement_one, completions, defects, task_tokens, turns_to_edit,
    /// diff_stats, delivery_friction) into one aggregation per query.
    metrics_trends: RuntimeMemo<(String, String)>,
    /// Per-server cache of the codebase analytics scan, one per workspace
    /// runtime: history-only analytics or related delivery facts per window.
    codebase: RuntimeMemo<Option<String>, orbit_core::application::codebase::CodebaseFacts>,
    /// Per-server cache of the task riskiness model, one per workspace
    /// runtime; task list rows and task detail estimate from it.
    riskiness: RuntimeMemo<(), orbit_core::application::codebase::TaskRiskinessModel>,
    /// Per-server cache of the agent scorecard scan, one per workspace
    /// runtime. Every role, window, detail and the attribution strip slice it.
    agents: RuntimeMemo<(), orbit_core::application::agents::AgentFacts>,
    /// Per-server cache of the last `orbit doctor` report for each workspace
    /// runtime, behind `/api/doctor`.
    doctor_reports: DoctorReports,
    /// `orbit web serve --operator` (and `orbit web connect` by default):
    /// stamp operator onto the dashboard session envelope. The only operator
    /// grant: the server's TTY and `ORBIT_OPERATOR` are ignored [ORB-23047].
    pub(in crate::state) operator: AtomicBool,
    /// Non-loopback Hosts the Host gate admits with a listed Tailscale login
    /// (`machine.web.trusted_hosts`/`trusted_logins`) and the activity seen
    /// through them. Set once at boot; unset admits loopback Hosts only.
    remote_access: std::sync::OnceLock<Arc<crate::state::RemoteAccess>>,
    /// Server-instance identifier (`web-operator-<uuid>`), reported as the
    /// caller on web task mutations when `--operator` is enabled.
    session_id: String,
    /// Test seam: paused just before a freshly-built runtime is published.
    #[cfg(test)]
    pub(super) on_pre_publish: Mutex<Option<PrePublishHook>>,
    /// Test-only fault injection for clock observations and changes.
    #[cfg(test)]
    clock_status_hook: Mutex<Option<ClockStatusHook>>,
    #[cfg(test)]
    clock_mutation_hook: Mutex<Option<ClockMutationHook>>,
}

/// Axum application state: the set of servable workspaces plus a lazy runtime
/// cache. Cheap to clone (single `Arc`).
#[derive(Clone)]
pub(crate) struct DashboardState {
    pub(super) inner: Arc<StateInner>,
}

impl DashboardState {
    /// Single-workspace mode: serve exactly one pre-built runtime, always
    /// selected. No longer reachable from `orbit web serve` (ORB-10029);
    /// used by [`crate::serve`] and by every handler test (which builds an
    /// in-memory runtime and wants a trivial single-workspace harness).
    #[cfg(test)]
    pub(crate) fn single(runtime: Arc<Core>) -> Self {
        Self::single_with_env(runtime, Env::default())
    }

    pub(crate) fn single_with_env(runtime: Arc<Core>, env: Env) -> Self {
        let entry = WsEntry {
            id: SINGLE_WORKSPACE_ID.to_string(),
            name: SINGLE_WORKSPACE_ID.to_string(),
            repo_root: PathBuf::new(),
            orbit_dir: PathBuf::new(),
            binding: Some(WorkspaceRuntimeBinding {
                logical_workspace_id: SINGLE_WORKSPACE_ID.to_string(),
                task_partition_id: SINGLE_WORKSPACE_ID.to_string(),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: PathBuf::new(),
                ship_mode: ShipMode::Local,
                base_branch: None,
            }),
            active: true,
            checkout_role: None,
            owner_machine_id: None,
        };
        // Nothing refuses a dashboard read, so both answer.
        let monitor = run_read(&runtime, run::Command::host_resource_monitor()).ok();
        let global_root = run_read(&runtime, run::Command::global_root()).unwrap_or_default();
        let mut runtimes = HashMap::new();
        runtimes.insert(
            SINGLE_WORKSPACE_ID.to_string(),
            CachedRuntime {
                binding: WorkspaceRuntimeBinding {
                    logical_workspace_id: SINGLE_WORKSPACE_ID.to_string(),
                    task_partition_id: SINGLE_WORKSPACE_ID.to_string(),
                    owner_machine_id: None,
                    checkout_role: None,
                    repo_root: PathBuf::new(),
                    ship_mode: ShipMode::Local,
                    base_branch: None,
                },
                orbit_dir: PathBuf::new(),
                generation: INITIAL_GENERATION,
                config_stamp: None,
                runtime,
            },
        );
        let state = Self::from_parts(
            env,
            global_root,
            SnapshotData {
                entries: vec![entry],
                default_workspace: Some(SINGLE_WORKSPACE_ID.to_string()),
            },
            runtimes,
            None,
            None,
        );
        *state
            .inner
            .host_resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = monitor;
        state
    }

    /// Registry-backed global mode: the servable workspace set is (re)loaded
    /// from `source` when [`DashboardState::refresh`] runs or when
    /// [`DashboardState::pin`] observes a registry or checkout fingerprint
    /// change, so native `orbit workspace init/remove`, binding changes, and
    /// checkout repair become visible without a restart. The initial load is
    /// eager — a malformed registry at startup is fatal (matching the
    /// pre-refresh behavior),
    /// whereas a later malformed refresh retains the last valid snapshot. An
    /// individual checkout with an unreadable identity is instead listed
    /// inactive so it cannot take down healthy workspaces.
    pub(crate) fn from_registry(
        global_root: PathBuf,
        source: RegistrySource,
        env: Env,
    ) -> Result<Self, OrbitError> {
        let loaded = source.load()?;
        Ok(Self::from_parts(
            env,
            global_root,
            loaded.data,
            HashMap::new(),
            Some(source),
            loaded.fingerprint,
        ))
    }

    fn from_parts(
        env: Env,
        global_root: PathBuf,
        snapshot: SnapshotData,
        runtimes: HashMap<String, CachedRuntime>,
        source: Option<RegistrySource>,
        last_fingerprint: Option<RegistryFingerprint>,
    ) -> Self {
        let host_disk_paths = runtimes
            .values()
            .filter_map(|cached| run_read(&cached.runtime, run::Command::host_disk_paths()).ok())
            .flatten()
            .collect();
        let checkout_fingerprints = checkout_fingerprints(&snapshot.entries);
        let initial = Snapshot {
            generation: INITIAL_GENERATION,
            entries: snapshot.entries,
            default_workspace: snapshot.default_workspace,
        };
        let hosts = HostFileState::new(global_root.clone(), &env);
        let host_tunnels = Arc::new(HostTunnels::new(TunnelConfig::from_env(&env)));
        Self {
            inner: Arc::new(StateInner {
                env,
                global_root,
                host_resources: Mutex::new(None),
                host_disk_paths,
                snapshot: Mutex::new(Arc::new(initial)),
                hosts,
                host_tunnels,
                runtimes: Mutex::new(runtimes),
                source,
                refresh_lock: Mutex::new(()),
                last_fingerprint: Mutex::new(last_fingerprint),
                last_checkout_fingerprints: Mutex::new(checkout_fingerprints),
                // Next successful refresh allocates INITIAL_GENERATION + 1.
                generation_counter: AtomicU64::new(INITIAL_GENERATION + 1),
                audit_summary: RuntimeMemo::new("audit summary aggregation"),
                cost: RuntimeMemo::new("managed-execution cost"),
                routines: RuntimeMemo::new("routine health"),
                plugin_panels: RuntimeMemo::new("plugin panel execution"),
                diagnostics_errors: RuntimeMemo::new("diagnostics errors aggregation"),
                diagnostics_friction: RuntimeMemo::new("diagnostics friction aggregation"),
                metrics_trends: RuntimeMemo::new("metrics trends aggregation"),
                codebase: RuntimeMemo::new("codebase analytics scan"),
                riskiness: RuntimeMemo::new("task riskiness model"),
                agents: RuntimeMemo::new("agent scorecard scan"),
                doctor_reports: DoctorReports::new(),
                operator: AtomicBool::new(false),
                remote_access: std::sync::OnceLock::new(),
                session_id: orbit_common::observability::audit_id::audit_execution_id(
                    "web-operator",
                ),
                #[cfg(test)]
                on_pre_publish: Mutex::new(None),
                #[cfg(test)]
                clock_status_hook: Mutex::new(None),
                #[cfg(test)]
                clock_mutation_hook: Mutex::new(None),
            }),
        }
    }

    /// Captured process environment handed in at server startup.
    pub(crate) fn env(&self) -> &Env {
        &self.inner.env
    }

    /// Global orbit root (`~/.orbit`) this server was launched against.
    /// In single mode, the supplied runtime's global root. Host-level views (routine
    /// scheduler health) read from here rather than any one workspace runtime,
    /// because routine fires live in the global store.
    pub(crate) fn global_root(&self) -> &std::path::Path {
        &self.inner.global_root
    }

    /// Sample the serving host independently of the request's workspace
    /// selector, against an already-pinned snapshot, so a combined read pays
    /// the registry/checkout fingerprint check once. This performs
    /// file/kernel reads and must be called through `blocking`.
    pub(crate) fn host_resource_status_pinned(
        &self,
        pinned: &Pinned,
    ) -> Result<orbit_core::runtime::host_resource::HostResourceStatus, OrbitError> {
        let mut paths = self.inner.host_disk_paths.clone();
        paths.push(self.inner.global_root.clone());
        for entry in &pinned.snapshot.entries {
            if entry.active && !entry.repo_root.as_os_str().is_empty() {
                paths.push(entry.repo_root.clone());
                paths.push(entry.orbit_dir.join("state/worktrees"));
            }
        }
        paths.retain(|path| !path.as_os_str().is_empty());
        paths.sort();
        paths.dedup();
        let monitor = {
            let mut guard = self
                .inner
                .host_resources
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if guard.is_none() {
                *guard = Some(Arc::new(orbit_core::composition::host_resource_monitor(
                    &self.inner.global_root,
                )?));
            }
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| OrbitError::Execution("host resource monitor unavailable".into()))?
        };
        Ok(monitor.snapshot(&paths))
    }

    /// The serving host's live provider usage readings and weekly account/crew
    /// pressures, as readiness lists them, for the top bar. The store is host-wide;
    /// thresholds and crew coverage come from the default workspace's effective config, so with no
    /// servable workspace (or an unreadable store) this is `None` and the bar
    /// shows the readings as unavailable, against an already-pinned
    /// snapshot. May read files and must be called through `blocking`.
    pub(crate) fn host_provider_limits_pinned(&self, pinned: &Pinned) -> Option<serde_json::Value> {
        let runtime = pinned.runtime_for(pinned.default_workspace()?).ok()?;
        match run_read(&runtime, run::Command::provider_limits(chrono::Utc::now())).ok()? {
            Ok(limits) => Some(limits),
            Err(error) => {
                tracing::warn!("top bar shows no provider usage: {error}");
                None
            }
        }
    }

    /// The serving host's host file: its last valid snapshot, reloaded when
    /// the file changes, and the error of a newer file that failed to load.
    /// May read files and must be called through `blocking`.
    pub(crate) fn hosts(&self) -> PinnedHosts {
        self.inner.hosts.pin()
    }

    /// The tunnels this dashboard owns to registered hosts. `run_server`
    /// stops them on shutdown and before a handover exec.
    pub(crate) fn host_tunnels(&self) -> &Arc<HostTunnels> {
        &self.inner.host_tunnels
    }

    /// Process-local `/api/audit/summary` memo for this server instance.
    pub(crate) fn audit_summary_memo(&self) -> &RuntimeMemo<String> {
        &self.inner.audit_summary
    }

    /// Process-local Cost page memo shared by every dashboard tab.
    pub(crate) fn cost_memo(&self) -> &RuntimeMemo<String> {
        &self.inner.cost
    }

    /// Host-wide routine memo shared even across different selected workspaces.
    pub(crate) fn routines_memo(&self) -> &RuntimeMemo<crate::runtime_memo::RoutineMemoKey> {
        &self.inner.routines
    }

    /// Process-local plugin panel memo shared by every dashboard tab.
    pub(crate) fn plugin_panel_memo(&self) -> &RuntimeMemo<(String, String)> {
        &self.inner.plugin_panels
    }

    /// Process-local codebase analytics scan cache for this server instance.
    pub(crate) fn codebase_memo(
        &self,
    ) -> &RuntimeMemo<Option<String>, orbit_core::application::codebase::CodebaseFacts> {
        &self.inner.codebase
    }

    /// Process-local task riskiness model cache for this server instance.
    pub(crate) fn riskiness_memo(
        &self,
    ) -> &RuntimeMemo<(), orbit_core::application::codebase::TaskRiskinessModel> {
        &self.inner.riskiness
    }

    /// Process-local agent scorecard scan cache for this server instance.
    pub(crate) fn agent_memo(
        &self,
    ) -> &RuntimeMemo<(), orbit_core::application::agents::AgentFacts> {
        &self.inner.agents
    }

    /// Process-local `/api/diagnostics/errors` memo for this server instance.
    pub(crate) fn diagnostics_errors_memo(&self) -> &RuntimeMemo<(String, usize)> {
        &self.inner.diagnostics_errors
    }

    /// Process-local `orbit doctor` report cache for this server instance.
    pub(crate) fn doctor_reports(&self) -> &DoctorReports {
        &self.inner.doctor_reports
    }

    /// Process-local `/api/diagnostics/friction` memo for this server instance.
    pub(crate) fn diagnostics_friction_memo(&self) -> &RuntimeMemo<(String, usize)> {
        &self.inner.diagnostics_friction
    }

    /// Process-local Analytics › Metrics trends memo for this server instance.
    pub(crate) fn metrics_trends_memo(&self) -> &RuntimeMemo<(String, String)> {
        &self.inner.metrics_trends
    }

    /// Whether this server was started with `--operator`, granting operator
    /// capability through the session envelope (the only operator grant; TTY
    /// and env are ignored).
    pub(crate) fn operator_session(&self) -> bool {
        self.inner.operator.load(Ordering::Relaxed)
    }

    /// The session every request to this server arrives on: it holds
    /// operator capability only when the server was started with
    /// `--operator`.
    pub(crate) fn session(&self) -> orbit_core::app::Session {
        orbit_core::app::Session::new(self.operator_session())
    }

    /// Record the `--operator` flag after construction. Set once at boot,
    /// before the server accepts requests.
    pub(crate) fn set_operator_session(&self, enabled: bool) {
        self.inner.operator.store(enabled, Ordering::Relaxed);
    }

    /// The Host gate's trusted-host policy and remote-access activity;
    /// loopback only unless [`Self::set_remote_access`] ran.
    pub(crate) fn remote_access(&self) -> Arc<crate::state::RemoteAccess> {
        self.inner
            .remote_access
            .get_or_init(|| Arc::new(crate::state::RemoteAccess::default()))
            .clone()
    }

    /// Record the remote-access policy. Set once at boot, before the app is
    /// built; a later call, or one after [`Self::remote_access`] was read,
    /// keeps the first.
    pub(crate) fn set_remote_access(&self, access: crate::state::RemoteAccess) {
        let _ = self.inner.remote_access.set(Arc::new(access));
    }

    /// Server-instance operator session identifier (`web-operator-<uuid>`) when
    /// this server runs with `--operator`, else `None`.
    pub(crate) fn operator_session_id(&self) -> Option<&str> {
        self.operator_session()
            .then_some(self.inner.session_id.as_str())
    }

    /// Observe the native host clock.
    pub(crate) fn clock_status(&self) -> Result<ClockStatus, OrbitError> {
        #[cfg(test)]
        if let Some(hook) = self
            .inner
            .clock_status_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
        {
            return hook();
        }
        orbit_core::application::routines::clock_status(&self.inner.global_root)
    }

    /// Install a test-only override for native clock observations.
    #[cfg(test)]
    pub(crate) fn set_clock_status_hook(&self, hook: ClockStatusHook) {
        *self
            .inner
            .clock_status_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// Install a test-only override for the native clock mutation.
    #[cfg(test)]
    pub(crate) fn set_clock_mutation_hook(&self, hook: ClockMutationHook) {
        *self
            .inner
            .clock_mutation_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    /// Run the test-only clock mutation override, if one is installed.
    #[cfg(test)]
    pub(crate) fn test_clock_mutation(&self) -> Option<Result<(), OrbitError>> {
        self.inner
            .clock_mutation_hook
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .map(|hook| hook())
    }

    /// Resolve (and lazily build + cache) the runtime for workspace `id` against
    /// the *live* snapshot. Test-only convenience: production resolves through
    /// [`Pinned::runtime_for`] so metadata and runtime share one generation.
    #[cfg(test)]
    pub(crate) fn runtime_for(&self, id: &str) -> Result<Arc<Core>, WsRejection> {
        let snapshot = self.inner.snapshot();
        self.inner.resolve_runtime(&snapshot, id)
    }

    /// Snapshot of the runtimes this server currently has open (built and
    /// cached) whose binding matches the live snapshot, in registry order.
    /// Test-only convenience: production health reads the pinned open set via
    /// [`Pinned::open_runtimes`].
    #[cfg(test)]
    pub(crate) fn open_runtimes(&self) -> Vec<(String, Arc<Core>)> {
        let snapshot = self.inner.snapshot();
        self.inner.open_runtimes_for(&snapshot)
    }

    /// Refresh from the authoritative registry when its fingerprint or a
    /// registered checkout's filesystem fingerprint has changed, then pin the
    /// resulting snapshot as one immutable [`Pinned`] view. Every derived read
    /// — default selection, entry metadata, runtime resolution, and the
    /// open-runtime set — sees the same generation, so a concurrent
    /// add/remove/rebind is observed as one coherent old-or-new response,
    /// never a mix. Unchanged fingerprints skip `load` and do not take
    /// [`StateInner::refresh_lock`].
    pub(crate) fn pin(&self) -> Pinned {
        self.reload_registry(false);
        Pinned {
            inner: self.inner.clone(),
            snapshot: self.inner.snapshot(),
        }
    }

    /// Resolve `requested` (or the default workspace) from the runtime
    /// cache alone, when that is already the whole answer `pin` +
    /// `resolve_runtime` would give: the registry and checkout
    /// fingerprints are current (so `pin` would not reload) and a cached
    /// runtime matches the current snapshot, binding and config stamp (so
    /// resolution would not build). Selection rejections are answers too.
    /// `None` means the request may need a reload or a build, which stays
    /// on the blocking path the extractors already use; this fast path
    /// only skips the thread hop every warm request used to pay. It runs on
    /// the request thread: the `stat` calls, the stamp comparison and the
    /// installed-plugin row query that `cached_matching` makes.
    pub(crate) fn try_resolve_cached(
        &self,
        requested: Option<&str>,
    ) -> Option<Result<Arc<Core>, WsRejection>> {
        if let Some(source) = self.inner.source.as_ref()
            && !self.registry_is_current(source)
        {
            return None;
        }
        let snapshot = self.inner.snapshot();
        let id = match requested {
            Some(id) => id,
            None => match snapshot.default_workspace.as_deref() {
                Some(id) => id,
                None => return Some(Err(WsRejection::no_default())),
            },
        };
        let (binding, orbit_dir) = match StateInner::snapshot_binding(&snapshot, id) {
            Ok(binding) => binding,
            Err(rejection) => return Some(Err(rejection)),
        };
        let config_stamp = ConfigLayerStamp::read(&self.inner.global_root, &orbit_dir);
        match self
            .inner
            .cached_matching(id, &binding, &orbit_dir, &config_stamp)
        {
            Ok(Some(runtime)) => Some(Ok(runtime)),
            Ok(None) => None,
            Err(rejection) => Some(Err(rejection)),
        }
    }

    /// Writes cannot rely on the last valid registry when a newer registry is
    /// unreadable. Reads retain their existing keep-last-valid behavior.
    pub(crate) fn pin_for_write(&self) -> Result<Pinned, OrbitError> {
        let pinned = self.pin();
        if let Some(source) = self.inner.source.as_ref()
            && !self.registry_is_current(source)
        {
            return Err(OrbitError::PolicyDenied(
                "current workspace registry is unavailable; refresh before writing".into(),
            ));
        }
        Ok(pinned)
    }

    /// Install the `#[cfg(test)]` pre-publish hook (see [`PrePublishHook`]).
    #[cfg(test)]
    pub(crate) fn set_pre_publish_hook(&self, hook: PrePublishHook) {
        *self
            .inner
            .on_pre_publish
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }
}
