//! Multi-workspace dashboard state (ORB-00030).
//!
//! The dashboard was originally coupled to a single `Arc<OrbitRuntime>` used
//! directly as axum state. To let one server serve every registered workspace
//! on the machine, state is generalized to a workspace-keyed, lazily-built
//! runtime map ([`DashboardState`]) and handlers receive their runtime through
//! the [`Ws`] extractor (which selects a workspace from the `?workspace=<id>`
//! query parameter, falling back to the configured default).
//!
//! [`DashboardState::single`] preserves the original single-workspace behavior:
//! one pre-built runtime, always selected, no lazy construction. `orbit web
//! serve` no longer reaches it (it always serves in global mode as of
//! ORB-10029); it is retained for [`crate::serve`] (callers embedding an
//! already-built `OrbitRuntime`) and for every existing handler test, which
//! builds an in-memory runtime and wants a trivial single-workspace harness.
//!
//! ## Concurrency model (ORB-10294)
//!
//! The registered workspace set is an immutable [`Snapshot`] stamped with a
//! monotonic **generation**, swapped atomically on [`DashboardState::refresh`].
//! The runtime cache is a *non-authoritative* memo: every read and every
//! publication is validated against an exact binding (runtime workspace id +
//! repo root + ship mode + `orbit_dir`) taken from a **pinned** snapshot
//! generation, so a runtime built
//! for an older snapshot can never be returned as current nor overwrite a newer
//! binding. Each request boundary pins one snapshot ([`DashboardState::pin`] →
//! [`Pinned`]) and derives default selection, entry metadata, runtime
//! resolution, and the open-runtime set from that single generation, so a
//! concurrent add/remove/rebind is observed as one coherent old-or-new view —
//! never old metadata spliced onto a newer runtime.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Json, Response};
use orbit_cmd::registry_runtime::{RegisteredRuntimeFactory, workspace_runtime_binding};
use orbit_core::application::routines::ClockStatus;
#[cfg(test)]
use orbit_core::application::routines::clock_status;
use orbit_core::runtime::{HostLifetime, WorkspaceRuntimeBinding};
use orbit_core::{OrbitError, OrbitRuntime, ShipMode};
use orbit_registry::workspace_registry;
use orbit_types::workspace::WorkspaceStatus;
use serde_json::json;

/// Synthetic workspace id used by [`DashboardState::single`].
pub(crate) const SINGLE_WORKSPACE_ID: &str = "default";

/// Generation assigned to the snapshot a [`DashboardState`] is constructed with.
/// Successful refreshes allocate strictly-increasing generations above it.
const INITIAL_GENERATION: u64 = 0;

/// A `#[cfg(test)]` seam invoked in `resolve_runtime` after a runtime is built
/// but *before* it is published to the cache. Lets a test deterministically
/// pause a build, mutate the registry, and refresh, then release the build to
/// prove an older-snapshot runtime cannot republish as current.
#[cfg(test)]
pub(crate) type PrePublishHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Test-only replacement for native host-clock observations. Production builds
/// call `clock_status` directly and do not carry this indirection.
#[cfg(test)]
pub(crate) type ClockStatusObserver =
    Arc<dyn Fn(&Path) -> Result<ClockStatus, OrbitError> + Send + Sync>;

/// One registered workspace the dashboard can serve.
///
/// `orbit_dir` is the workspace's `.orbit` directory — the value passed to
/// [`RegisteredRuntimeFactory::open_resolved_checkout`] as the workspace root. Active
/// entries carry the complete runtime binding resolved from the logical
/// workspace and local checkout. Inactive entries — whether their checkout path
/// is stale or its identity cannot be read — keep no binding: they are listed
/// but never built.
#[derive(Clone, Debug)]
pub(crate) struct WsEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) repo_root: PathBuf,
    pub(crate) orbit_dir: PathBuf,
    pub(crate) binding: Option<WorkspaceRuntimeBinding>,
    pub(crate) active: bool,
}

/// Immutable, atomically-swapped view of the registered workspace set plus the
/// dropdown's default selection. A refresh ([`DashboardState::refresh`])
/// replaces the whole `Arc<Snapshot>` in one step, so a reader either sees the
/// old view or the new one — never a half-applied update.
///
/// `generation` is a monotonic identity assigned when the snapshot is published.
/// It lets the runtime cache reject an older-snapshot build that would otherwise
/// overwrite a newer binding, and lets a pinned request prove which generation
/// it is reading (see the module-level concurrency model).
pub(crate) struct Snapshot {
    generation: u64,
    entries: Vec<WsEntry>,
    default_workspace: Option<String>,
}

/// Where a refresh reloads the servable workspace set from. Present only in the
/// registry-backed mode built by [`crate::build_state`]; [`DashboardState::single`]
/// and [`DashboardState::global`] leave it `None`, making [`DashboardState::refresh`]
/// a no-op (their entries are supplied directly and never re-read).
pub(crate) struct RegistrySource {
    /// Path to the served registry: `<--root>/workspaces.json` when an
    /// explicit root was given, `~/.orbit/workspaces.json` otherwise (or a
    /// test double).
    registry_path: PathBuf,
    /// The `--workspace <selector>` flag, if any, for default re-selection.
    workspace_selector: Option<String>,
    /// Process cwd captured at startup, for default re-selection.
    cwd: Option<PathBuf>,
    /// Per-checkout failures already reported to operators. A refresh happens at
    /// every request boundary, so retaining this set avoids emitting the same
    /// diagnostic for every request while allowing a repaired-and-broken-again
    /// checkout to be reported anew.
    reported_unavailable: Mutex<HashSet<UnavailableCheckout>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct UnavailableCheckout {
    workspace: String,
    checkout: PathBuf,
    error: String,
}

impl UnavailableCheckout {
    fn warn(&self) {
        tracing::warn!(
            workspace = %self.workspace,
            checkout = %self.checkout.display(),
            error = %self.error,
            "registered workspace is unavailable; dashboard will continue serving healthy workspaces"
        );
    }
}

impl RegistrySource {
    pub(crate) fn new(
        registry_path: PathBuf,
        workspace_selector: Option<String>,
        cwd: Option<PathBuf>,
    ) -> Self {
        Self {
            registry_path,
            workspace_selector,
            cwd,
            reported_unavailable: Mutex::new(HashSet::new()),
        }
    }

    /// Reload the authoritative registry into a fresh (generation-less) snapshot
    /// view. Stale-path workspaces are marked inactive (never deleted) via
    /// `validate_workspaces`; active checkouts whose identity cannot be resolved
    /// are likewise excluded after an operator-visible warning. The caller stamps
    /// the generation at publication.
    fn load(&self) -> Result<SnapshotData, OrbitError> {
        let mut registry = workspace_registry::load_registry_from(&self.registry_path)?;
        workspace_registry::validate_workspaces(&mut registry);
        let mut unavailable = HashSet::new();
        let entries: Vec<WsEntry> = workspace_registry::local_workspaces(&registry)
            .map(|(workspace, checkout)| {
                let binding = (workspace.status == WorkspaceStatus::Active)
                    .then(|| workspace_runtime_binding(workspace, checkout))
                    .and_then(|result| match result {
                        Ok(binding) => Some(binding),
                        Err(error) => {
                            unavailable.insert(UnavailableCheckout {
                                workspace: workspace.id.clone(),
                                checkout: checkout.orbit_dir.clone(),
                                error: error.to_string(),
                            });
                            None
                        }
                    });
                let active = binding.is_some();
                WsEntry {
                    id: workspace.id.clone(),
                    name: workspace.name.clone(),
                    repo_root: checkout.repo_root.clone(),
                    orbit_dir: checkout.orbit_dir.clone(),
                    binding,
                    active,
                }
            })
            .collect();
        self.report_unavailable(unavailable);
        let default_workspace = crate::default_workspace_selection(
            &registry,
            self.workspace_selector.as_deref(),
            self.cwd.as_deref(),
        )
        .filter(|id| entries.iter().any(|entry| entry.id == *id && entry.active));
        Ok(SnapshotData {
            entries,
            default_workspace,
        })
    }

    fn report_unavailable(&self, unavailable: HashSet<UnavailableCheckout>) {
        let new_failures = {
            let mut reported = self
                .reported_unavailable
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            reported.retain(|failure| unavailable.contains(failure));
            unavailable
                .into_iter()
                .filter(|failure| reported.insert(failure.clone()))
                .collect::<Vec<_>>()
        };
        for failure in new_failures {
            failure.warn();
        }
    }
}

/// A loaded-but-unpublished snapshot: the workspace set and default selection
/// without a generation. [`StateInner::publish_snapshot`] stamps a generation
/// and wraps it in an `Arc<Snapshot>`.
struct SnapshotData {
    entries: Vec<WsEntry>,
    default_workspace: Option<String>,
}

/// A built runtime plus the binding *and generation* it was constructed for.
/// The binding lets a refresh evict a runtime whose workspace was rebound
/// (root/orbit-dir changed) and lets every cache read reject a stale entry; the
/// generation lets publication refuse to overwrite a newer binding with an
/// older-snapshot build.
struct CachedRuntime {
    binding: WorkspaceRuntimeBinding,
    orbit_dir: PathBuf,
    generation: u64,
    runtime: Arc<OrbitRuntime>,
}

struct StateInner {
    /// The served Orbit root: an explicit `--root`, else `~/.orbit`. Passed as
    /// `global_root` when building per-workspace runtimes. Unused in single
    /// mode.
    global_root: PathBuf,
    /// Atomically-swapped registered workspace set + default selection.
    snapshot: Mutex<Arc<Snapshot>>,
    /// Lazily-built, cached runtimes keyed by workspace id.
    runtimes: Mutex<HashMap<String, CachedRuntime>>,
    /// Registry to reload from on refresh; `None` disables refresh (single /
    /// direct-entry global modes).
    source: Option<RegistrySource>,
    /// Serializes refreshes so a snapshot swap and its runtime eviction are one
    /// atomic step relative to other refreshes. Never held across runtime
    /// construction.
    refresh_lock: Mutex<()>,
    /// Allocates strictly-increasing generations for published snapshots.
    generation_counter: AtomicU64,
    /// Test seam: paused just before a freshly-built runtime is published.
    #[cfg(test)]
    on_pre_publish: Mutex<Option<PrePublishHook>>,
    /// Test seam for deterministic host-clock reads in global API fixtures.
    #[cfg(test)]
    clock_status_observer: Mutex<ClockStatusObserver>,
}

impl StateInner {
    fn lock_snapshot(&self) -> std::sync::MutexGuard<'_, Arc<Snapshot>> {
        self.snapshot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_runtimes(&self) -> std::sync::MutexGuard<'_, HashMap<String, CachedRuntime>> {
        // Recover from poisoning: the cache is an idempotent build cache, so a
        // panic in another thread cannot leave it logically inconsistent.
        self.runtimes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Cheap clone of the live snapshot `Arc`, taken under (and released with)
    /// the snapshot lock so callers hold no lock while reading it.
    fn snapshot(&self) -> Arc<Snapshot> {
        self.lock_snapshot().clone()
    }

    /// Stamp `data` with the next generation and wrap it for publication.
    fn publish_snapshot(&self, data: SnapshotData) -> Snapshot {
        Snapshot {
            generation: self.generation_counter.fetch_add(1, Ordering::Relaxed),
            entries: data.entries,
            default_workspace: data.default_workspace,
        }
    }

    /// Resolve (and lazily build + cache) the runtime for `id` against a single
    /// pinned `snapshot`. The pinned snapshot is the sole authority for the
    /// binding: the cache is validated against it, never trusted by id alone.
    ///
    /// Building happens outside every lock. Publication (`publish_runtime`)
    /// refuses to overwrite a newer-generation binding, so a runtime built for
    /// an older snapshot is returned only to *this* request's pinned view and
    /// never becomes the current cache entry.
    fn resolve_runtime(
        &self,
        snapshot: &Snapshot,
        id: &str,
    ) -> Result<Arc<OrbitRuntime>, WsRejection> {
        let (binding, orbit_dir) = {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.id == id)
                .ok_or_else(|| WsRejection::unknown(id))?;
            if !entry.active {
                return Err(WsRejection::inactive(id));
            }
            let binding = entry
                .binding
                .clone()
                .ok_or_else(|| WsRejection::missing_binding(id))?;
            (binding, entry.orbit_dir.clone())
        };
        let generation = snapshot.generation;

        // Fast path: a cached runtime whose binding still matches this snapshot.
        if let Some(runtime) = self.cached_matching(id, &binding, &orbit_dir) {
            return Ok(runtime);
        }

        // Build outside the lock (no lock held across construction).
        let runtime = RegisteredRuntimeFactory::open_resolved_checkout_for(
            &self.global_root,
            &orbit_dir,
            &orbit_dir,
            binding.clone(),
            HostLifetime::LongLived,
        )
        .map_err(|e| WsRejection::build_failed(id, &e))?;
        let runtime = Arc::new(runtime);

        // Test seam: pause between build and publish so a test can rebind +
        // refresh and prove this (now older-generation) build cannot republish.
        #[cfg(test)]
        self.invoke_pre_publish_hook(id);

        Ok(self.publish_runtime(id, binding, orbit_dir, generation, runtime))
    }

    /// Publish a freshly-built runtime under the cache lock with binding +
    /// generation discipline, returning the runtime that is authoritative for
    /// the caller's pinned generation.
    fn publish_runtime(
        &self,
        id: &str,
        binding: WorkspaceRuntimeBinding,
        orbit_dir: PathBuf,
        generation: u64,
        runtime: Arc<OrbitRuntime>,
    ) -> Arc<OrbitRuntime> {
        let mut cache = self.lock_runtimes();
        if let Some(existing) = cache.get(id) {
            // Same binding: a concurrent build already won; it is idempotent.
            if existing.binding == binding && existing.orbit_dir == orbit_dir {
                return existing.runtime.clone();
            }
            // Different binding at an equal-or-newer generation than ours means
            // a newer snapshot already published here; an older-snapshot build
            // must never overwrite it. Return it for this request's pinned old
            // generation only (a differing binding cannot share our generation,
            // since one generation has one binding per id).
            if generation < existing.generation {
                return runtime;
            }
        }
        cache.insert(
            id.to_string(),
            CachedRuntime {
                binding,
                orbit_dir,
                generation,
                runtime: runtime.clone(),
            },
        );
        runtime
    }

    /// Return the cached runtime for `id` iff its complete runtime binding and
    /// orbit directory match the pinned entry; a mismatch (including a
    /// workspace-id or ship-mode-only change) reports absent so the caller
    /// rebuilds against the pinned snapshot.
    fn cached_matching(
        &self,
        id: &str,
        binding: &WorkspaceRuntimeBinding,
        orbit_dir: &Path,
    ) -> Option<Arc<OrbitRuntime>> {
        let cache = self.lock_runtimes();
        cache.get(id).and_then(|cached| {
            (cached.binding == *binding && cached.orbit_dir == orbit_dir)
                .then(|| cached.runtime.clone())
        })
    }

    /// The open (built + cached) runtimes whose binding matches `snapshot`, in
    /// snapshot order. Joining by exact binding — not by id — is what prevents a
    /// stale cache entry from being surfaced or tagged as the wrong checkout.
    fn open_runtimes_for(&self, snapshot: &Snapshot) -> Vec<(String, Arc<OrbitRuntime>)> {
        let cache = self.lock_runtimes();
        snapshot
            .entries
            .iter()
            .filter_map(|entry| {
                cache.get(&entry.id).and_then(|cached| {
                    (entry.binding.as_ref() == Some(&cached.binding)
                        && cached.orbit_dir == entry.orbit_dir)
                        .then(|| (entry.id.clone(), cached.runtime.clone()))
                })
            })
            .collect()
    }

    #[cfg(test)]
    fn invoke_pre_publish_hook(&self, id: &str) {
        let hook = self
            .on_pre_publish
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook(id);
        }
    }
}

/// Axum application state: the set of servable workspaces plus a lazy runtime
/// cache. Cheap to clone (single `Arc`).
#[derive(Clone)]
pub(crate) struct DashboardState {
    inner: Arc<StateInner>,
}

impl DashboardState {
    /// Single-workspace mode: serve exactly one pre-built runtime, always
    /// selected. No longer reachable from `orbit web serve` (ORB-10029);
    /// used by [`crate::serve`] and by every handler test (which builds an
    /// in-memory runtime and wants a trivial single-workspace harness).
    pub(crate) fn single(runtime: Arc<OrbitRuntime>) -> Self {
        let entry = WsEntry {
            id: SINGLE_WORKSPACE_ID.to_string(),
            name: SINGLE_WORKSPACE_ID.to_string(),
            repo_root: PathBuf::new(),
            orbit_dir: PathBuf::new(),
            binding: Some(WorkspaceRuntimeBinding {
                logical_workspace_id: SINGLE_WORKSPACE_ID.to_string(),
                task_partition_id: SINGLE_WORKSPACE_ID.to_string(),
                owner_machine_id: None,
                repo_root: PathBuf::new(),
                ship_mode: ShipMode::Local,
                base_branch: None,
            }),
            active: true,
        };
        let mut runtimes = HashMap::new();
        runtimes.insert(
            SINGLE_WORKSPACE_ID.to_string(),
            CachedRuntime {
                binding: WorkspaceRuntimeBinding {
                    logical_workspace_id: SINGLE_WORKSPACE_ID.to_string(),
                    task_partition_id: SINGLE_WORKSPACE_ID.to_string(),
                    owner_machine_id: None,
                    repo_root: PathBuf::new(),
                    ship_mode: ShipMode::Local,
                    base_branch: None,
                },
                orbit_dir: PathBuf::new(),
                generation: INITIAL_GENERATION,
                runtime,
            },
        );
        Self::from_parts(
            PathBuf::new(),
            SnapshotData {
                entries: vec![entry],
                default_workspace: Some(SINGLE_WORKSPACE_ID.to_string()),
            },
            runtimes,
            None,
        )
    }

    /// Global mode with an explicitly-supplied entry set (no registry reload).
    /// Used by handler tests; `default_workspace` (if any) is the workspace
    /// selected when a request omits `?workspace=`. [`DashboardState::refresh`]
    /// is a no-op here — the entries are fixed at construction. Production
    /// serving uses [`DashboardState::from_registry`] instead.
    #[cfg(test)]
    pub(crate) fn global(
        global_root: PathBuf,
        entries: Vec<WsEntry>,
        default_workspace: Option<String>,
    ) -> Self {
        Self::from_parts(
            global_root,
            SnapshotData {
                entries,
                default_workspace,
            },
            HashMap::new(),
            None,
        )
    }

    /// Registry-backed global mode: the servable workspace set is (re)loaded
    /// from `source` on every [`DashboardState::refresh`], so native `orbit
    /// workspace init/remove` and binding changes become visible without a
    /// restart. The initial load is eager — a malformed registry at startup is
    /// fatal (matching the pre-refresh behavior), whereas a later malformed
    /// refresh retains the last valid snapshot. An individual checkout with an
    /// unreadable identity is instead listed inactive so it cannot take down
    /// healthy workspaces.
    pub(crate) fn from_registry(
        global_root: PathBuf,
        source: RegistrySource,
    ) -> Result<Self, OrbitError> {
        let snapshot = source.load()?;
        Ok(Self::from_parts(
            global_root,
            snapshot,
            HashMap::new(),
            Some(source),
        ))
    }

    fn from_parts(
        global_root: PathBuf,
        snapshot: SnapshotData,
        runtimes: HashMap<String, CachedRuntime>,
        source: Option<RegistrySource>,
    ) -> Self {
        let initial = Snapshot {
            generation: INITIAL_GENERATION,
            entries: snapshot.entries,
            default_workspace: snapshot.default_workspace,
        };
        Self {
            inner: Arc::new(StateInner {
                global_root,
                snapshot: Mutex::new(Arc::new(initial)),
                runtimes: Mutex::new(runtimes),
                source,
                refresh_lock: Mutex::new(()),
                // Next successful refresh allocates INITIAL_GENERATION + 1.
                generation_counter: AtomicU64::new(INITIAL_GENERATION + 1),
                #[cfg(test)]
                on_pre_publish: Mutex::new(None),
                #[cfg(test)]
                clock_status_observer: Mutex::new(Arc::new(clock_status)),
            }),
        }
    }

    /// The currently-servable workspace entries (a cheap clone of the live
    /// snapshot). Test-only convenience: production reads go through
    /// [`DashboardState::pin`] so metadata and runtime share one generation.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<WsEntry> {
        self.inner.snapshot().entries.clone()
    }

    /// Global orbit root (`~/.orbit`) this server was launched against. Empty
    /// in single mode ([`DashboardState::single`]). Host-level views (routine
    /// scheduler health) read from here rather than any one workspace runtime,
    /// because routine fires live in the global store.
    pub(crate) fn global_root(&self) -> &std::path::Path {
        &self.inner.global_root
    }

    /// Observe the native host clock. Production retains the direct native
    /// call; unit tests may replace only this read boundary.
    pub(crate) fn clock_status(&self) -> Result<ClockStatus, OrbitError> {
        #[cfg(test)]
        {
            let observer = self
                .inner
                .clock_status_observer
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            observer(&self.inner.global_root)
        }
        #[cfg(not(test))]
        {
            orbit_core::application::routines::clock_status(&self.inner.global_root)
        }
    }

    /// Test-only convenience: the live default selection. Production reads the
    /// pinned default via [`Pinned::default_workspace`].
    #[cfg(test)]
    pub(crate) fn default_workspace(&self) -> Option<String> {
        self.inner.snapshot().default_workspace.clone()
    }

    /// Resolve (and lazily build + cache) the runtime for workspace `id` against
    /// the *live* snapshot. Test-only convenience: production resolves through
    /// [`Pinned::runtime_for`] so metadata and runtime share one generation.
    #[cfg(test)]
    pub(crate) fn runtime_for(&self, id: &str) -> Result<Arc<OrbitRuntime>, WsRejection> {
        let snapshot = self.inner.snapshot();
        self.inner.resolve_runtime(&snapshot, id)
    }

    /// Snapshot of the runtimes this server currently has open (built and
    /// cached) whose binding matches the live snapshot, in registry order.
    /// Test-only convenience: production health reads the pinned open set via
    /// [`Pinned::open_runtimes`].
    #[cfg(test)]
    pub(crate) fn open_runtimes(&self) -> Vec<(String, Arc<OrbitRuntime>)> {
        let snapshot = self.inner.snapshot();
        self.inner.open_runtimes_for(&snapshot)
    }

    /// Refresh from the authoritative registry, then pin the resulting snapshot
    /// as one immutable [`Pinned`] view. Every derived read — default selection,
    /// entry metadata, runtime resolution, and the open-runtime set — sees the
    /// same generation, so a concurrent add/remove/rebind is observed as one
    /// coherent old-or-new response, never a mix.
    pub(crate) fn pin(&self) -> Pinned {
        self.refresh();
        Pinned {
            inner: self.inner.clone(),
            snapshot: self.inner.snapshot(),
        }
    }

    /// Reload the registered workspace set from the authoritative registry and
    /// reconcile the runtime cache. A no-op unless this state was built via
    /// [`DashboardState::from_registry`].
    ///
    /// Guarantees:
    /// - **Atomic swap.** The new snapshot replaces the old one in a single
    ///   assignment under the snapshot lock; readers never observe a partial
    ///   update.
    /// - **Keep-last-valid.** A malformed or unreadable registry leaves the
    ///   current snapshot untouched and emits a credential-safe diagnostic.
    /// - **No build under lock.** Eviction only drops cache entries; runtimes
    ///   are (re)built lazily in `resolve_runtime`, never here and never while a
    ///   registry/cache lock is held.
    pub(crate) fn refresh(&self) {
        let Some(source) = self.inner.source.as_ref() else {
            return;
        };
        // Serialize concurrent refreshes so the swap + eviction below is one
        // atomic step. Held across the registry read but never across runtime
        // construction (which only happens in `resolve_runtime`, off this lock).
        let _serialize = self
            .inner
            .refresh_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let data = match source.load() {
            Ok(data) => data,
            Err(error) => {
                // A malformed or partially-written registry must never replace
                // a good in-memory snapshot. The diagnostic names the registry
                // path and Orbit's own error message; it deliberately never
                // echoes the file's contents, so a tokenized `git_remote` in
                // the registry cannot leak into logs.
                let diagnostic = RefreshFailure::new(&source.registry_path, &error);
                diagnostic.warn();
                return;
            }
        };
        // Publish a generation-stamped snapshot. Newer generation than any cache
        // entry built before this point, so `publish_runtime` treats an
        // in-flight older build as stale.
        let snapshot = self.inner.publish_snapshot(data);
        // Bindings still servable after the swap; used to evict runtimes whose
        // workspace was removed, went inactive, or was rebound.
        let live: Vec<(String, WorkspaceRuntimeBinding, PathBuf)> = snapshot
            .entries
            .iter()
            .filter(|entry| entry.active)
            .filter_map(|entry| {
                entry
                    .binding
                    .clone()
                    .map(|binding| (entry.id.clone(), binding, entry.orbit_dir.clone()))
            })
            .collect();
        {
            let mut guard = self.inner.lock_snapshot();
            *guard = Arc::new(snapshot);
        }
        let mut cache = self.inner.lock_runtimes();
        cache.retain(|id, cached| {
            live.iter().any(|(live_id, binding, orbit_dir)| {
                live_id == id && *binding == cached.binding && *orbit_dir == cached.orbit_dir
            })
        });
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

    /// Replace host-clock observations before this state is cloned into a
    /// router. Clock control still uses the native mutation functions.
    #[cfg(test)]
    pub(crate) fn with_clock_status_observer(self, observer: ClockStatusObserver) -> Self {
        *self
            .inner
            .clock_status_observer
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = observer;
        self
    }
}

/// A request-pinned view of dashboard state: one immutable [`Snapshot`]
/// generation plus shared access to the runtime cache. Every read and every
/// runtime resolution is evaluated against this single snapshot, so one response
/// never mixes old entry metadata with a runtime resolved from a newer binding.
pub(crate) struct Pinned {
    inner: Arc<StateInner>,
    snapshot: Arc<Snapshot>,
}

impl Pinned {
    /// The pinned generation's servable workspace entries.
    pub(crate) fn entries(&self) -> &[WsEntry] {
        &self.snapshot.entries
    }

    /// The pinned generation's default-workspace selection.
    pub(crate) fn default_workspace(&self) -> Option<&str> {
        self.snapshot.default_workspace.as_deref()
    }

    /// Resolve the runtime for `id` against the pinned snapshot's exact binding.
    pub(crate) fn runtime_for(&self, id: &str) -> Result<Arc<OrbitRuntime>, WsRejection> {
        self.inner.resolve_runtime(&self.snapshot, id)
    }

    /// Open runtimes whose binding matches the pinned snapshot, in snapshot
    /// order — the coherent open set for this request's generation.
    pub(crate) fn open_runtimes(&self) -> Vec<(String, Arc<OrbitRuntime>)> {
        self.inner.open_runtimes_for(&self.snapshot)
    }
}

/// The structured, credential-safe diagnostic emitted when a registry refresh
/// fails. It carries only the registry *path* and Orbit's own error text —
/// never the file contents — so a tokenized `git_remote` in the registry cannot
/// leak into logs. Extracted so its fields are unit-testable without a
/// subscriber and so the `warn!` call site emits exactly these two values.
pub(crate) struct RefreshFailure {
    registry: String,
    error: String,
}

impl RefreshFailure {
    fn new(registry_path: &Path, error: &OrbitError) -> Self {
        Self {
            registry: registry_path.display().to_string(),
            error: error.to_string(),
        }
    }

    fn warn(&self) {
        tracing::warn!(
            registry = %self.registry,
            error = %self.error,
            "workspace registry refresh failed; retaining last valid workspace set"
        );
    }
}

/// Rejection returned by the [`Ws`] extractor when a workspace cannot be
/// selected or built. Renders as a JSON `{ "error": ... }` body.
#[derive(Debug)]
pub(crate) struct WsRejection {
    status: StatusCode,
    message: String,
}

impl WsRejection {
    fn unknown(id: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: format!("unknown workspace: {id}"),
        }
    }

    fn inactive(id: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: format!("workspace '{id}' is inactive; select an active workspace"),
        }
    }

    fn build_failed(id: &str, err: &OrbitError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("failed to open workspace '{id}': {err}"),
        }
    }

    fn missing_binding(id: &str) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("active workspace '{id}' has no runtime binding"),
        }
    }

    fn no_default() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: "no workspace selected and no default is configured; \
                      pass ?workspace=<id>"
                .to_string(),
        }
    }
}

impl IntoResponse for WsRejection {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

/// Extractor yielding the `Arc<OrbitRuntime>` for the request's workspace.
///
/// Selection order: the `?workspace=<id>` query parameter, else the state's
/// configured default. Handlers destructure it as `Ws(runtime)` — a drop-in
/// replacement for the former `State(runtime): State<Arc<OrbitRuntime>>`.
pub(crate) struct Ws(pub(crate) Arc<OrbitRuntime>);

#[axum::async_trait]
impl FromRequestParts<DashboardState> for Ws {
    type Rejection = WsRejection;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &DashboardState,
    ) -> Result<Self, Self::Rejection> {
        // Refresh and pin one snapshot so selection and runtime resolution share
        // a generation: a native add/remove/rebind since the last request is
        // honored, and the resolved runtime always matches the pinned binding.
        let requested = parts.uri.query().and_then(workspace_from_query);
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            let pinned = state.pin();
            let id = match requested {
                Some(id) => id,
                None => pinned
                    .default_workspace()
                    .map(str::to_string)
                    .ok_or_else(WsRejection::no_default)?,
            };
            Ok(Ws(pinned.runtime_for(&id)?))
        })
        .await
        .map_err(|error| WsRejection {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: format!("workspace selection panicked: {error}"),
        })?
    }
}

/// Extract the `workspace` value from a raw query string (percent-decoded),
/// ignoring empty values so `?workspace=` behaves like an omitted parameter.
fn workspace_from_query(query: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(k, _)| k == "workspace")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
}
