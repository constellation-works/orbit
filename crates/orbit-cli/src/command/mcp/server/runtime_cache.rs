//! Long-lived workspace runtime reuse and invalidation.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use orbit_cmd::registry_runtime::{RegisteredRuntimeStamp, ResolvedWorkspaceSelection};
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_types::workspace::{Workspace, WorkspaceCheckout};

/// Tracing target and message emitted once per workspace runtime this process
/// opens.
///
/// Together they are the observable seam for "this server reuses what it
/// opens": the MCP integration test enables this target and counts the lines
/// across calls, so both strings are matched there literally.
pub(super) const RUNTIME_OPEN_LOG_TARGET: &str = "orbit.mcp.runtime";
pub(super) const OPENED_WORKSPACE_RUNTIME_LOG: &str = "opened a workspace runtime";

/// The runtimes this process has built, keyed by logical workspace ID.
///
/// [`orbit_core::runtime::HostLifetime::LongLived`] promises the host keeps what it opens, but the
/// server used to drop each runtime at the end of the call that built it — so
/// every workspace-scoped call re-parsed the host identity, reopened the task
/// registry and all workspace stores, and started a fresh embed worker. An
/// entry is reused only while everything it was composed from still holds: the
/// registry records this call resolved, plus a [`RegisteredRuntimeStamp`] over
/// the files behind them and over both `config.toml` layers, so an edit to the
/// runtime configuration takes effect on the next call rather than at the next
/// server restart.
pub(super) struct WorkspaceRuntimeCache {
    entries: Mutex<HashMap<String, CachedRuntime>>,
}

/// One built runtime together with the facts it was composed from.
struct CachedRuntime {
    workspace: Workspace,
    checkout: WorkspaceCheckout,
    stamp: RegisteredRuntimeStamp,
    value: Arc<OrbitRuntime>,
}

impl CachedRuntime {
    /// `ResolvedWorkspaceSelection::local_root` is deliberately not compared:
    /// [`orbit_cmd::registry_runtime::RegisteredRuntimeFactory::open_registered_checkout_for`] composes
    /// against the registered checkout's own `.orbit` for both roots, so a
    /// selection that differs only there yields the same runtime.
    fn matches(
        &self,
        selected: &ResolvedWorkspaceSelection,
        stamp: &RegisteredRuntimeStamp,
    ) -> bool {
        self.workspace == selected.workspace
            && self.checkout == selected.checkout
            && self.stamp == *stamp
    }
}

impl Default for WorkspaceRuntimeCache {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl WorkspaceRuntimeCache {
    /// Reuse the runtime already built for `selected`, or build one and keep it.
    ///
    /// `build` runs outside the cache lock: opening stores blocks on I/O, and
    /// two concurrent builds for the same facts are interchangeable.
    pub(super) fn resolve(
        &self,
        global_root: &Path,
        selected: &ResolvedWorkspaceSelection,
        build: impl FnOnce() -> Result<OrbitRuntime, OrbitError>,
    ) -> Result<Arc<OrbitRuntime>, OrbitError> {
        let stamp = RegisteredRuntimeStamp::read(global_root, &selected.checkout);
        if let Some(value) = self.reusable(selected, &stamp) {
            return Ok(value);
        }
        let value = Arc::new(build()?);
        let mut entries = self.lock();
        // A racing call may have published an equivalent entry while this one
        // built; prefer the published runtime so the session converges on one.
        if let Some(published) = entries
            .get(&selected.workspace.id)
            .filter(|cached| cached.matches(selected, &stamp))
        {
            return Ok(Arc::clone(&published.value));
        }
        // Otherwise this build becomes the entry, replacing whatever stale one
        // a rebind or an edited registry left behind for this workspace.
        entries.insert(
            selected.workspace.id.clone(),
            CachedRuntime {
                workspace: selected.workspace.clone(),
                checkout: selected.checkout.clone(),
                stamp,
                value: Arc::clone(&value),
            },
        );
        Ok(value)
    }

    /// The cached runtime for this selection iff every fact it was built from
    /// is unchanged. A mismatch reports absent, so the caller rebuilds.
    fn reusable(
        &self,
        selected: &ResolvedWorkspaceSelection,
        stamp: &RegisteredRuntimeStamp,
    ) -> Option<Arc<OrbitRuntime>> {
        self.lock()
            .get(&selected.workspace.id)
            .filter(|cached| cached.matches(selected, stamp))
            .map(|cached| Arc::clone(&cached.value))
    }

    /// Poisoning is recoverable here: the map is an idempotent build cache, so
    /// a panic in another call cannot leave it logically inconsistent.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, CachedRuntime>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
