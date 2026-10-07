//! The serving host's host file as a generation-swapped snapshot [ORB-14451].
//!
//! Settings › Hosts lists from the last valid load of `hosts.toml` (or the
//! legacy destinations file) under the same rule as the workspace registry
//! snapshot: each request `stat`s the files the load reads, reloads only when
//! one changed, and swaps in a new generation only when the load succeeds. A
//! file that fails to load leaves the last valid snapshot in place and is
//! reported beside it, so a bad hand edit becomes a banner, not an empty view.
//! Mutations never read this snapshot: they call the CLI's operations, which
//! load the file themselves and refuse a concurrent edit.

use orbit_registry::hosts::{
    HostRegistry, hosts_path, legacy_destinations_path, load_host_registry, validated_host_root,
};
use orbit_registry::machine_identity::CONFIG_TOML_FILE;
use serde::Serialize;

use super::*;

/// One published load of the host file.
pub(crate) struct HostSnapshot {
    pub(crate) generation: u64,
    pub(crate) registry: HostRegistry,
}

/// Why the newest host file did not load, in the CLI's error vocabulary.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct HostLoadError {
    pub(crate) code: String,
    pub(crate) message: String,
}

/// One request's view: the last valid snapshot, and the error of a newer file
/// that failed to load. Both absent only before the first load attempt.
pub(crate) struct PinnedHosts {
    pub(crate) snapshot: Option<Arc<HostSnapshot>>,
    pub(crate) load_error: Option<HostLoadError>,
}

/// `stat` identity of every file the load reads: the host file, the legacy
/// destinations file and the global `config.toml` holding `[machine]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HostFileFingerprint([Option<(SystemTime, u64)>; 3]);

pub(crate) struct HostFileState {
    global_root: PathBuf,
    current: Mutex<Option<Arc<HostSnapshot>>>,
    load_error: Mutex<Option<HostLoadError>>,
    /// Sampled before the last successful load; a failed load leaves it
    /// untouched so the next request retries.
    last_fingerprint: Mutex<Option<HostFileFingerprint>>,
    /// Serializes reloads so a swap and its error state change together.
    refresh_lock: Mutex<()>,
    generation_counter: AtomicU64,
}

impl HostFileState {
    pub(crate) fn new(global_root: PathBuf) -> Self {
        Self {
            global_root,
            current: Mutex::new(None),
            load_error: Mutex::new(None),
            last_fingerprint: Mutex::new(None),
            refresh_lock: Mutex::new(()),
            generation_counter: AtomicU64::new(INITIAL_GENERATION),
        }
    }

    /// Reload when a file the load reads has changed, then return the last
    /// valid snapshot and any newer load error as one view.
    pub(crate) fn pin(&self) -> PinnedHosts {
        if !self.is_current() {
            let _serialize = self
                .refresh_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if !self.is_current() {
                self.reload();
            }
        }
        PinnedHosts {
            snapshot: lock(&self.current).clone(),
            load_error: lock(&self.load_error).clone(),
        }
    }

    fn is_current(&self) -> bool {
        *lock(&self.last_fingerprint) == Some(self.fingerprint())
    }

    /// An unusable root has no fingerprint, so its load error is retried.
    fn fingerprint(&self) -> HostFileFingerprint {
        let Ok(root) = validated_host_root(&self.global_root) else {
            return HostFileFingerprint([None; 3]);
        };
        let stat = |path: PathBuf| {
            let metadata = std::fs::metadata(path).ok()?;
            Some((metadata.modified().ok()?, metadata.len()))
        };
        HostFileFingerprint([
            stat(hosts_path(&root)),
            stat(legacy_destinations_path(&root)),
            stat(root.join(CONFIG_TOML_FILE)),
        ])
    }

    fn reload(&self) {
        // Sampled before the read, so a rewrite during the load forces the
        // next request to reload rather than pairing new metadata with old data.
        let fingerprint = self.fingerprint();
        match validated_host_root(&self.global_root).and_then(|root| load_host_registry(&root)) {
            Ok(registry) => {
                let snapshot = HostSnapshot {
                    generation: self.generation_counter.fetch_add(1, Ordering::Relaxed),
                    registry,
                };
                *lock(&self.current) = Some(Arc::new(snapshot));
                *lock(&self.load_error) = None;
                *lock(&self.last_fingerprint) = Some(fingerprint);
            }
            Err(error) => {
                tracing::warn!(
                    root = %self.global_root.display(),
                    %error,
                    "host file failed to load; the dashboard keeps its last valid snapshot"
                );
                *lock(&self.load_error) = Some(HostLoadError {
                    code: crate::api::host_error_code(&error).to_string(),
                    message: error.to_string(),
                });
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
