use super::super::probe::CachedHostResourceProbe;
use super::super::{HostResourceProbe, HostResourceSample, RESOURCE_CACHE_TTL};
use super::fixtures::sample;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct ManualClock {
    origin: Instant,
    offset_ms: AtomicU64,
}

impl ManualClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
            offset_ms: AtomicU64::new(0),
        }
    }

    fn advance(&self, by: Duration) {
        self.offset_ms.fetch_add(
            u64::try_from(by.as_millis()).expect("offset fits"),
            Ordering::SeqCst,
        );
    }

    fn instant(&self) -> Instant {
        self.origin + Duration::from_millis(self.offset_ms.load(Ordering::SeqCst))
    }
}

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
    let clock = Arc::new(ManualClock::new());
    let now = {
        let clock = Arc::clone(&clock);
        Arc::new(move || clock.instant())
    };
    let cache = Arc::new(CachedHostResourceProbe::with_clock(source.clone(), now));
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
    clock.advance(RESOURCE_CACHE_TTL);
    cache.sample(&[PathBuf::from("/other")]);
    assert_eq!(
        source.0.load(Ordering::SeqCst),
        3,
        "expired unknown samples must be retried"
    );
}
