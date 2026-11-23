use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use orbit_cmd::registry_runtime::{ConfigLayerStamp, RegisteredRuntimeFactory};
use orbit_core::runtime::{HostLifetime, WorkspaceRuntimeBinding};
use orbit_core::{OrbitError, OrbitRuntime};

use super::super::{Snapshot, SnapshotData, WsRejection};
use super::StateInner;

/// A built runtime plus the binding *and generation* it was constructed for.
/// The binding lets a refresh evict a runtime whose workspace was rebound
/// (root/orbit-dir changed) and lets every cache read reject a stale entry; the
/// generation lets publication refuse to overwrite a newer binding with an
/// older-snapshot build.
pub(super) struct CachedRuntime {
    pub(super) binding: WorkspaceRuntimeBinding,
    pub(super) orbit_dir: PathBuf,
    pub(super) generation: u64,
    /// Config layers the runtime was built from, sampled before its build.
    /// A later edit to either `config.toml` makes this stale so the next
    /// request rebuilds instead of serving frozen settings indefinitely.
    /// `None` marks a caller-supplied runtime: its lifecycle belongs to the
    /// caller, and its synthetic dashboard binding must never be used to
    /// reconstruct it (including after config or plugin changes).
    pub(super) config_stamp: Option<ConfigLayerStamp>,
    pub(super) runtime: Arc<OrbitRuntime>,
}

impl StateInner {
    pub(super) fn lock_snapshot(&self) -> std::sync::MutexGuard<'_, Arc<Snapshot>> {
        self.snapshot.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn lock_runtimes(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, CachedRuntime>> {
        // Recover from poisoning: the cache is an idempotent build cache, so a
        // panic in another thread cannot leave it logically inconsistent.
        self.runtimes.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Cheap clone of the live snapshot `Arc`, taken under (and released with)
    /// the snapshot lock so callers hold no lock while reading it.
    pub(super) fn snapshot(&self) -> Arc<Snapshot> {
        self.lock_snapshot().clone()
    }

    /// Stamp `data` with the next generation and wrap it for publication.
    pub(super) fn publish_snapshot(&self, data: SnapshotData) -> Snapshot {
        Snapshot {
            generation: self.generation_counter.fetch_add(1, Ordering::Relaxed),
            entries: data.entries,
            default_workspace: data.default_workspace,
        }
    }

    /// The binding and orbit dir `snapshot` serves `id` under, or the
    /// rejection its selection earns. The sole authority for that mapping:
    /// strict resolution, the config-file resolver and the cached fast path
    /// all read it, so they can never disagree about a workspace.
    pub(super) fn snapshot_binding(
        snapshot: &Snapshot,
        id: &str,
    ) -> Result<(WorkspaceRuntimeBinding, PathBuf), WsRejection> {
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
        Ok((binding, entry.orbit_dir.clone()))
    }

    /// Resolve (and lazily build + cache) the runtime for `id` against a single
    /// pinned `snapshot`. The pinned snapshot is the sole authority for the
    /// binding: the cache is validated against it, never trusted by id alone.
    ///
    /// Building happens outside every lock. Publication (`publish_runtime`)
    /// refuses to overwrite a newer-generation binding, so a runtime built for
    /// an older snapshot is returned only to *this* request's pinned view and
    /// never becomes the current cache entry.
    pub(in crate::state) fn resolve_runtime(
        &self,
        snapshot: &Snapshot,
        id: &str,
    ) -> Result<Arc<OrbitRuntime>, WsRejection> {
        let (binding, orbit_dir) = Self::snapshot_binding(snapshot, id)?;
        let generation = snapshot.generation;
        // Sample before the build, as the registry fingerprint is: an edit
        // during construction then makes the published entry stale on the
        // next request rather than current beside older data.
        let config_stamp = ConfigLayerStamp::read(&self.global_root, &orbit_dir);

        // Fast path: a cached runtime whose binding still matches this snapshot.
        if let Some(runtime) = self.cached_matching(id, &binding, &orbit_dir, &config_stamp)? {
            return Ok(runtime);
        }

        // Build outside the lock (no lock held across construction).
        // A broken layered config is an error here so aggregates stay
        // explicit `unavailable` rows instead of silently serving the stale
        // runtime. The config file view has its own stale-tolerant resolver
        // (`resolve_runtime_for_config_file`) so a broken layer stays
        // repairable there with its 400 + path.
        let runtime = Arc::new(
            RegisteredRuntimeFactory::open_resolved_checkout_for(
                &self.global_root,
                &orbit_dir,
                &orbit_dir,
                binding.clone(),
                HostLifetime::LongLived,
            )
            .map_err(|error| WsRejection::build_failed(id, &error))?,
        );

        // Test seam: pause between build and publish so a test can rebind +
        // refresh and prove this (now older-generation) build cannot republish.
        #[cfg(test)]
        self.invoke_pre_publish_hook(id);

        Ok(self.publish_runtime(id, binding, orbit_dir, generation, config_stamp, runtime))
    }

    /// Stale-tolerant resolver for `GET /api/config/file` only.
    ///
    /// The strict [`Self::resolve_runtime`] reports a broken layered config
    /// as an error so fleet and aggregate rows stay explicit `unavailable`
    /// entries. The file view instead must stay reachable: it reads the files
    /// directly and reports a 400 naming the file path. When the rebuild
    /// fails with a layered-config input error and a cached runtime for the
    /// same binding and orbit dir exists, serve that stale runtime so the
    /// file view can render its 400; otherwise report the build failure.
    pub(in crate::state) fn resolve_runtime_for_config_file(
        &self,
        snapshot: &Snapshot,
        id: &str,
    ) -> Result<Arc<OrbitRuntime>, WsRejection> {
        let (binding, orbit_dir) = Self::snapshot_binding(snapshot, id)?;
        let generation = snapshot.generation;
        let config_stamp = ConfigLayerStamp::read(&self.global_root, &orbit_dir);
        if let Some(runtime) = self.cached_matching(id, &binding, &orbit_dir, &config_stamp)? {
            return Ok(runtime);
        }
        match RegisteredRuntimeFactory::open_resolved_checkout_for(
            &self.global_root,
            &orbit_dir,
            &orbit_dir,
            binding.clone(),
            HostLifetime::LongLived,
        ) {
            Ok(runtime) => {
                let runtime = Arc::new(runtime);
                #[cfg(test)]
                self.invoke_pre_publish_hook(id);
                Ok(self.publish_runtime(id, binding, orbit_dir, generation, config_stamp, runtime))
            }
            Err(error)
                if matches!(
                    error,
                    OrbitError::InvalidInput(_)
                        | OrbitError::InvalidInputDiagnostic { .. }
                        | OrbitError::UnknownWorkspaceSelector(_)
                ) =>
            {
                let cache = self.lock_runtimes();
                if let Some(cached) = cache.get(id)
                    && cached.binding == binding
                    && cached.orbit_dir == orbit_dir
                {
                    return Ok(cached.runtime.clone());
                }
                Err(WsRejection::build_failed(id, &error))
            }
            Err(error) => Err(WsRejection::build_failed(id, &error)),
        }
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
        config_stamp: ConfigLayerStamp,
        runtime: Arc<OrbitRuntime>,
    ) -> Arc<OrbitRuntime> {
        let mut cache = self.lock_runtimes();
        if let Some(existing) = cache.get(id) {
            // Same binding and config: a concurrent build already won; it is
            // idempotent. A different stamp is stale, so it falls through to
            // replacement under the generation rule below.
            if existing.binding == binding
                && existing.orbit_dir == orbit_dir
                && existing.config_stamp.as_ref() == Some(&config_stamp)
            {
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
                config_stamp: Some(config_stamp),
                runtime: runtime.clone(),
            },
        );
        runtime
    }

    /// Return the cached runtime for `id` iff its complete runtime binding and
    /// orbit directory match the pinned entry; a mismatch (including a
    /// workspace-id or ship-mode-only change) reports absent so the caller
    /// rebuilds against the pinned snapshot.
    pub(super) fn cached_matching(
        &self,
        id: &str,
        binding: &WorkspaceRuntimeBinding,
        orbit_dir: &Path,
        config_stamp: &ConfigLayerStamp,
    ) -> Result<Option<Arc<OrbitRuntime>>, WsRejection> {
        let (runtime, config_current) = {
            let cache = self.lock_runtimes();
            match cache.get(id) {
                Some(cached)
                    if cached.binding == *binding
                        && cached.orbit_dir == orbit_dir
                        && cached.config_stamp.is_none() =>
                {
                    return Ok(Some(cached.runtime.clone()));
                }
                Some(cached) if cached.binding == *binding && cached.orbit_dir == orbit_dir => (
                    Some(cached.runtime.clone()),
                    cached.config_stamp.as_ref() == Some(config_stamp),
                ),
                _ => (None, false),
            }
        };
        let Some(runtime) = runtime else {
            return Ok(None);
        };
        if !config_current {
            // A config-layer edit: keep the stale entry so a rebuild that
            // fails on the broken layer can fall back to it in
            // `resolve_runtime` (the file view stays repairable). A
            // successful rebuild replaces it via `publish_runtime`.
            return Ok(None);
        }
        // The stamp equality above covers the workspace plugin toggles,
        // which live in the stamped workspace config file; only the host
        // plugin rows still need comparing here.
        if !runtime
            .plugin_installed_changed()
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
    pub(in crate::state) fn open_runtimes_for(
        &self,
        snapshot: &Snapshot,
    ) -> Vec<(String, Arc<OrbitRuntime>)> {
        // Sample config stamps before the cache lock, so the steady state
        // stays a set of `stat` calls and no file I/O happens under it. A
        // stale entry is not surfaced as current; the next per-workspace
        // request rebuilds it through `resolve_runtime`.
        let stamps: Vec<ConfigLayerStamp> = snapshot
            .entries
            .iter()
            .map(|entry| ConfigLayerStamp::read(&self.global_root, &entry.orbit_dir))
            .collect();
        let cache = self.lock_runtimes();
        snapshot
            .entries
            .iter()
            .zip(stamps.iter())
            .filter_map(|(entry, stamp)| {
                cache.get(&entry.id).and_then(|cached| {
                    (entry.binding.as_ref() == Some(&cached.binding)
                        && cached.orbit_dir == entry.orbit_dir
                        && cached
                            .config_stamp
                            .as_ref()
                            .is_none_or(|cached_stamp| cached_stamp == stamp))
                    .then(|| (entry.id.clone(), cached.runtime.clone()))
                })
            })
            .collect()
    }

    #[cfg(test)]
    pub(super) fn invoke_pre_publish_hook(&self, id: &str) {
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
