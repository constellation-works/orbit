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
//! never old metadata spliced onto a newer runtime. `pin` reloads the registry
//! file only when its mtime or length, or a registered checkout's filesystem
//! fingerprint, has changed; the steady state is a set of `stat` calls plus
//! an `Arc` clone, not a serialized `load`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

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

use crate::runtime_memo::RuntimeMemo;

mod dashboard;
mod registry;
mod request;

#[cfg(test)]
mod tests;

pub(crate) use dashboard::DashboardState;
use dashboard::StateInner;

pub(crate) use registry::RegistrySource;
use registry::{CheckoutFingerprint, RegistryFingerprint, checkout_fingerprints};
pub(crate) use request::{Pinned, RefreshFailure, Ws, WsRejection};

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

/// A loaded-but-unpublished snapshot: the workspace set and default selection
/// without a generation. [`StateInner::publish_snapshot`] stamps a generation
/// and wraps it in an `Arc<Snapshot>`.
struct SnapshotData {
    entries: Vec<WsEntry>,
    default_workspace: Option<String>,
}
