//! Single-flight host observations shared by runtime callers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Instant;

use super::{HostResourceProbe, HostResourceSample, RESOURCE_CACHE_TTL, platform};

/// Single-flight, bounded (one path set) cache, also usable around an injected probe.
pub struct CachedHostResourceProbe {
    source: Arc<dyn HostResourceProbe>,
    cached: Mutex<Option<(Instant, Vec<PathBuf>, HostResourceSample)>>,
}

impl CachedHostResourceProbe {
    pub fn new(source: Arc<dyn HostResourceProbe>) -> Self {
        Self {
            source,
            cached: Mutex::new(None),
        }
    }
}

impl HostResourceProbe for CachedHostResourceProbe {
    fn sample(&self, disk_paths: &[PathBuf]) -> HostResourceSample {
        let mut guard = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, paths, sample)) = &*guard
            && at.elapsed() < RESOURCE_CACHE_TTL
            && paths == disk_paths
        {
            return sample.clone();
        }
        let sample = self.source.sample(disk_paths);
        *guard = Some((Instant::now(), disk_paths.to_vec(), sample.clone()));
        sample
    }
}

/// The process shares its native probe so runtime clones and callers reuse samples.
pub fn default_host_resource_probe() -> Arc<dyn HostResourceProbe> {
    static PROBE: OnceLock<Arc<platform::NativeProbe>> = OnceLock::new();
    PROBE
        .get_or_init(|| Arc::new(platform::NativeProbe::default()))
        .clone()
}
