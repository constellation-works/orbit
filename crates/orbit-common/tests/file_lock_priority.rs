//! Writer preference for advisory file locks shared by overlapping readers.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

orbit_common::isolate_test_process!();

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use orbit_common::fs::io::{
    FileLockOptions, with_exclusive_file_lock_options, with_shared_file_lock_options,
};

const READERS: u64 = 4;
const READ_SECTION: Duration = Duration::from_millis(40);

fn preferring_writers() -> FileLockOptions {
    FileLockOptions {
        timeout: Duration::from_secs(5),
        warn_after: Duration::from_secs(5),
        prefer_exclusive_waiters: true,
        ..FileLockOptions::default()
    }
}

/// Readers whose sections overlap without a gap: each re-enters as soon as it
/// leaves, and their phases are staggered so another always holds the lock.
struct OverlappingReaders {
    stop: Arc<AtomicBool>,
    sections: Arc<AtomicU64>,
    threads: Vec<std::thread::JoinHandle<Result<(), std::io::Error>>>,
}

impl OverlappingReaders {
    fn start(target: &Path) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let sections = Arc::new(AtomicU64::new(0));
        let threads = (0..READERS)
            .map(|reader| {
                let target = target.to_path_buf();
                let stop = Arc::clone(&stop);
                let sections = Arc::clone(&sections);
                std::thread::spawn(move || {
                    std::thread::sleep(READ_SECTION / READERS as u32 * reader as u32);
                    while !stop.load(Ordering::Relaxed) {
                        with_shared_file_lock_options(
                            &target,
                            "reader",
                            preferring_writers(),
                            || {
                                std::thread::sleep(READ_SECTION);
                                Ok::<_, std::io::Error>(())
                            },
                        )?;
                        sections.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok(())
                })
            })
            .collect();
        let readers = Self {
            stop,
            sections,
            threads,
        };
        readers.wait_for_sections(READERS * 4);
        readers
    }

    fn wait_for_sections(&self, at_least: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.sections.load(Ordering::Relaxed) < at_least {
            assert!(
                Instant::now() < deadline,
                "readers completed only {} of {at_least} sections",
                self.sections.load(Ordering::Relaxed)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads {
            thread
                .join()
                .expect("reader thread")
                .expect("a reader is delayed behind a writer, never timed out");
        }
    }
}

/// Criterion: an exclusive waiter acquires within a bounded time while shared
/// sections keep overlapping. Before ORB-15107 the writer got the lock only at
/// an instant when no reader held it, which these readers never leave, so it
/// waited out its whole deadline.
#[test]
fn an_exclusive_waiter_acquires_while_shared_sections_keep_overlapping() {
    let dir = tempfile::tempdir().expect("dir");
    let target = dir.path().join("partition");
    let readers = OverlappingReaders::start(&target);

    for _ in 0..3 {
        let queued = Instant::now();
        let waited =
            with_exclusive_file_lock_options(&target, "writer", preferring_writers(), || {
                Ok::<_, std::io::Error>(queued.elapsed())
            })
            .expect("the writer acquires before its deadline");
        // The readers already inside drain within one section; the rest is
        // retry pacing. The bound leaves room for a loaded host while staying
        // well inside the 5 s deadline the writer used to wait out.
        assert!(
            waited < Duration::from_secs(2),
            "the writer waited {waited:?} behind overlapping readers"
        );

        // Readers held back behind the writer resume once it leaves.
        let resumed = readers.sections.load(Ordering::Relaxed) + READERS;
        readers.wait_for_sections(resumed);
    }

    readers.stop();
}
