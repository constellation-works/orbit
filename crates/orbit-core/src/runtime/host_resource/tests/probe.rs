use super::super::probe::CachedHostResourceProbe;
use super::super::{HostResourceProbe, HostResourceSample, RESOURCE_CACHE_TTL};
use super::fixtures::sample;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn overlapping_callers_reuse_one_sample_including_unknown() {
    struct Counter(AtomicUsize);
    impl HostResourceProbe for Counter {
        fn sample(&self, _: &[PathBuf]) -> HostResourceSample {
            self.0.fetch_add(1, Ordering::SeqCst);
            sample(0, None, None, None)
        }
    }
    let source = Arc::new(Counter(AtomicUsize::new(0)));
    let cache = Arc::new(CachedHostResourceProbe::new(source.clone()));
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let cache = cache.clone();
            scope.spawn(move || {
                assert!(
                    cache
                        .sample(&[PathBuf::from("/workspace")])
                        .cpu_percent
                        .is_none()
                )
            });
        }
    });
    assert_eq!(source.0.load(Ordering::SeqCst), 1);
    cache.sample(&[PathBuf::from("/other")]);
    assert_eq!(
        source.0.load(Ordering::SeqCst),
        2,
        "different filesystems need their own observations"
    );
    std::thread::sleep(RESOURCE_CACHE_TTL + std::time::Duration::from_millis(30));
    cache.sample(&[PathBuf::from("/other")]);
    assert_eq!(
        source.0.load(Ordering::SeqCst),
        3,
        "expired unknown samples must be retried"
    );
}
