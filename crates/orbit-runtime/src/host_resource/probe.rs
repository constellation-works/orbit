//! Single-flight host observations shared by runtime callers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use super::{HostResourceProbe, HostResourceSample, RESOURCE_CACHE_TTL, platform};

/// Single-flight, bounded (one path set) cache, also usable around an injected probe.
pub struct CachedHostResourceProbe {
    source: Arc<dyn HostResourceProbe>,
    cached: Mutex<Option<(Instant, Vec<PathBuf>, HostResourceSample)>>,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
}

impl CachedHostResourceProbe {
    pub fn new(source: Arc<dyn HostResourceProbe>) -> Self {
        Self::with_clock(source, Arc::new(Instant::now))
    }

    /// `now` is the monotonic clock the TTL is measured with.
    /// [`Self::new`] reads [`Instant::now`].
    pub fn with_clock(
        source: Arc<dyn HostResourceProbe>,
        now: Arc<dyn Fn() -> Instant + Send + Sync>,
    ) -> Self {
        Self {
            source,
            cached: Mutex::new(None),
            now,
        }
    }
}

impl HostResourceProbe for CachedHostResourceProbe {
    fn sample(&self, disk_paths: &[PathBuf]) -> HostResourceSample {
        let mut guard = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        let fresh = (self.now)();
        if let Some((at, paths, sample)) = &*guard
            && fresh.saturating_duration_since(*at) < RESOURCE_CACHE_TTL
            && paths == disk_paths
        {
            return sample.clone();
        }
        let sample = self.source.sample(disk_paths);
        *guard = Some(((self.now)(), disk_paths.to_vec(), sample.clone()));
        sample
    }
}

/// Native probe whose cache TTL follows `now` instead of the process clock.
///
/// Production callers use [`default_host_resource_probe`]. Tests advance `now`
/// to expire a sample without sleeping out [`RESOURCE_CACHE_TTL`].
#[doc(hidden)]
pub fn host_resource_probe_with_clock(
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
) -> Arc<dyn HostResourceProbe> {
    Arc::new(super::platform::NativeProbe::with_clock(now))
}

/// The process shares its native probe so runtime clones and callers reuse samples.
pub fn default_host_resource_probe() -> Arc<dyn HostResourceProbe> {
    static PROBE: OnceLock<Arc<platform::NativeProbe>> = OnceLock::new();
    PROBE
        .get_or_init(|| Arc::new(platform::NativeProbe::default()))
        .clone()
}

/// Native probe with an injected procfs root and cache clock for fixture observations.
/// Linux reads `loadavg`, `stat`, `meminfo`, and `pressure/io` beneath this root.
#[doc(hidden)]
pub fn host_resource_probe_with_proc_root(
    root: PathBuf,
    now: Arc<dyn Fn() -> Instant + Send + Sync>,
) -> Arc<dyn HostResourceProbe> {
    Arc::new(platform::NativeProbe::with_proc_root(root, now))
}
