use super::*;

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

pub(super) struct StateInner {
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
    /// construction. The request-path fast path (`pin` with unchanged registry
    /// and checkout fingerprints) does not take this lock.
    refresh_lock: Mutex<()>,
    /// Last successfully loaded `workspaces.json` mtime+len. Compared on `pin`
    /// without `refresh_lock`; updated only after a successful snapshot swap.
    last_fingerprint: Mutex<Option<RegistryFingerprint>>,
    /// Last successfully observed filesystem state for the registered
    /// checkouts in the current snapshot. Compared on `pin` without
    /// `refresh_lock` so disappeared and repaired checkouts are revalidated
    /// without rereading a steady-state registry.
    last_checkout_fingerprints: Mutex<Vec<CheckoutFingerprint>>,
    /// Allocates strictly-increasing generations for published snapshots.
    generation_counter: AtomicU64,
    /// Per-server memo for `/api/audit/summary`. Keyed by runtime identity
    /// and the raw `since` window so relative cutoffs (`24h`) still hit.
    audit_summary: RuntimeMemo<String>,
    /// Per-server single-flight TTL memo for audited plugin panel reads.
    /// Keyed by `(namespace, panel)`.
    plugin_panels: RuntimeMemo<(String, String)>,
    /// `orbit web serve --operator` (and `orbit web connect` by default):
    /// stamp operator onto the dashboard session envelope regardless of TTY
    /// or `ORBIT_OPERATOR`.
    operator: AtomicBool,
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
    pub(super) fn resolve_runtime(
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
        if let Some(runtime) = self.cached_matching(id, &binding, &orbit_dir)? {
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
    ) -> Result<Option<Arc<OrbitRuntime>>, WsRejection> {
        let runtime = {
            let cache = self.lock_runtimes();
            cache.get(id).and_then(|cached| {
                (cached.binding == *binding && cached.orbit_dir == orbit_dir)
                    .then(|| cached.runtime.clone())
            })
        };
        let Some(runtime) = runtime else {
            return Ok(None);
        };
        if !runtime
            .plugin_state_changed()
            .map_err(|error| WsRejection::build_failed(id, &error))?
        {
            return Ok(Some(runtime));
        }

        // A lifecycle verb rewrote the host plugin rows after this runtime's
        // load pass. Remove only the exact stale entry observed above: a
        // concurrent request may already have published its replacement.
        let mut cache = self.lock_runtimes();
        if cache
            .get(id)
            .is_some_and(|cached| Arc::ptr_eq(&cached.runtime, &runtime))
        {
            cache.remove(id);
        }
        Ok(None)
    }

    /// The open (built + cached) runtimes whose binding matches `snapshot`, in
    /// snapshot order. Joining by exact binding — not by id — is what prevents a
    /// stale cache entry from being surfaced or tagged as the wrong checkout.
    pub(super) fn open_runtimes_for(
        &self,
        snapshot: &Snapshot,
    ) -> Vec<(String, Arc<OrbitRuntime>)> {
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
        let checkout_fingerprints = checkout_fingerprints(&snapshot.entries);
        let initial = Snapshot {
            generation: INITIAL_GENERATION,
            entries: snapshot.entries,
            default_workspace: snapshot.default_workspace,
        };
        let last_fingerprint = source.as_ref().and_then(RegistrySource::fingerprint);
        Self {
            inner: Arc::new(StateInner {
                global_root,
                snapshot: Mutex::new(Arc::new(initial)),
                runtimes: Mutex::new(runtimes),
                source,
                refresh_lock: Mutex::new(()),
                last_fingerprint: Mutex::new(last_fingerprint),
                last_checkout_fingerprints: Mutex::new(checkout_fingerprints),
                // Next successful refresh allocates INITIAL_GENERATION + 1.
                generation_counter: AtomicU64::new(INITIAL_GENERATION + 1),
                audit_summary: RuntimeMemo::new("audit summary aggregation"),
                plugin_panels: RuntimeMemo::new("plugin panel execution"),
                operator: AtomicBool::new(false),
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

    /// Process-local `/api/audit/summary` memo for this server instance.
    pub(crate) fn audit_summary_memo(&self) -> &RuntimeMemo<String> {
        &self.inner.audit_summary
    }

    /// Process-local plugin panel memo shared by every dashboard tab.
    pub(crate) fn plugin_panel_memo(&self) -> &RuntimeMemo<(String, String)> {
        &self.inner.plugin_panels
    }

    /// Whether this server was started with `--operator`, granting operator
    /// capability through the session envelope (independent of TTY / env).
    pub(crate) fn operator_session(&self) -> bool {
        self.inner.operator.load(Ordering::Relaxed)
    }

    /// Record the `--operator` flag after construction. Set once at boot,
    /// before the server accepts requests.
    pub(crate) fn set_operator_session(&self, enabled: bool) {
        self.inner.operator.store(enabled, Ordering::Relaxed);
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

    /// Reload the registered workspace set from the authoritative registry and
    /// reconcile the runtime cache. A no-op unless this state was built via
    /// [`DashboardState::from_registry`]. Always re-reads the file. Request
    /// handlers should use [`DashboardState::pin`], which skips `load`
    /// when the registry and checkout fingerprints are unchanged. Production
    /// request paths only call `pin`; tests (and any future admin force-reload)
    /// use this method.
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
    #[cfg_attr(not(test), expect(dead_code))]
    pub(crate) fn refresh(&self) {
        self.reload_registry(true);
    }

    /// `force` bypasses the registry and checkout fingerprint gates so explicit
    /// [`DashboardState::refresh`] still re-validates checkouts even when
    /// `workspaces.json` and the checkout paths themselves are unchanged. The
    /// request path (`pin`) passes `false`.
    fn reload_registry(&self, force: bool) {
        let Some(source) = self.inner.source.as_ref() else {
            return;
        };
        if !force && self.registry_is_current(source) {
            return;
        }
        // Serialize concurrent reloads so the swap + eviction below is one
        // atomic step. Held across the registry read but never across runtime
        // construction (which only happens in `resolve_runtime`, off this lock).
        // The unchanged-fingerprint fast path above never reaches this lock.
        let _serialize = self
            .inner
            .refresh_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !force && self.registry_is_current(source) {
            return;
        }
        let data = match source.load() {
            Ok(data) => data,
            Err(error) => {
                // A malformed or partially-written registry must never replace
                // a good in-memory snapshot. The diagnostic names the registry
                // path and Orbit's own error message; it deliberately never
                // echoes the file's contents, so a tokenized `git_remote` in
                // the registry cannot leak into logs. Leave the fingerprint
                // untouched so the next pin retries.
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
        let checkout_fingerprint_state = checkout_fingerprints(&snapshot.entries);
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
        {
            let mut cache = self.inner.lock_runtimes();
            cache.retain(|id, cached| {
                live.iter().any(|(live_id, binding, orbit_dir)| {
                    live_id == id && *binding == cached.binding && *orbit_dir == cached.orbit_dir
                })
            });
        }
        // Record the post-load fingerprint so the next pin can skip. Done after
        // the snapshot swap so a concurrent pin cannot observe a new
        // fingerprint against the old snapshot.
        self.store_fingerprint(source.fingerprint());
        self.store_checkout_fingerprints(checkout_fingerprint_state);
    }

    fn registry_is_current(&self, source: &RegistrySource) -> bool {
        let Some(current) = source.fingerprint() else {
            return false;
        };
        if self.lock_fingerprint().as_ref() != Some(&current) {
            return false;
        }
        let snapshot = self.inner.snapshot();
        let current_checkouts = checkout_fingerprints(&snapshot.entries);
        self.lock_checkout_fingerprints().as_slice() == current_checkouts.as_slice()
    }

    fn store_fingerprint(&self, fingerprint: Option<RegistryFingerprint>) {
        *self.lock_fingerprint() = fingerprint;
    }

    fn lock_fingerprint(&self) -> std::sync::MutexGuard<'_, Option<RegistryFingerprint>> {
        self.inner
            .last_fingerprint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn store_checkout_fingerprints(&self, fingerprints: Vec<CheckoutFingerprint>) {
        *self.lock_checkout_fingerprints() = fingerprints;
    }

    fn lock_checkout_fingerprints(&self) -> std::sync::MutexGuard<'_, Vec<CheckoutFingerprint>> {
        self.inner
            .last_checkout_fingerprints
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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

    /// Successful registry `load` calls, including the eager `from_registry`
    /// construction. Single/global modes that have no source report 0.
    #[cfg(test)]
    pub(crate) fn registry_load_count(&self) -> u64 {
        self.inner
            .source
            .as_ref()
            .map(|source| source.load_count.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    /// Acquire `refresh_lock` so a test can prove `pin` does not wait on it
    /// when the registry and checkout fingerprints are unchanged.
    #[cfg(test)]
    pub(crate) fn lock_refresh(&self) -> std::sync::MutexGuard<'_, ()> {
        self.inner
            .refresh_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}
