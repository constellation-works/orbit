use std::path::PathBuf;
use std::sync::{Arc, PoisonError};

use orbit_core::app::workspace::WorkspaceRuntimeBinding;

use super::super::{
    CheckoutFingerprint, RefreshFailure, RegistryFingerprint, RegistrySource, checkout_fingerprints,
};
use super::DashboardState;

impl DashboardState {
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
    #[cfg(test)]
    pub(crate) fn refresh(&self) {
        self.reload_registry(true);
    }

    /// `force` bypasses the registry and checkout fingerprint gates so explicit
    /// `DashboardState::refresh` still re-validates checkouts even when
    /// `workspaces.json` and the checkout paths themselves are unchanged. The
    /// request path (`pin`) passes `false`.
    pub(super) fn reload_registry(&self, force: bool) {
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
        let loaded = match source.load() {
            Ok(loaded) => loaded,
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
        let snapshot = self.inner.publish_snapshot(loaded.data);
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
        // This fingerprint was sampled before the registry read. A rewrite
        // during load or publication therefore forces the next pin to reload
        // instead of caching newer file metadata beside older snapshot data.
        self.store_fingerprint(loaded.fingerprint);
        self.store_checkout_fingerprints(checkout_fingerprint_state);
    }

    pub(super) fn registry_is_current(&self, source: &RegistrySource) -> bool {
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
}
