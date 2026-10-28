//! Bounded fan-out for a sweep's per-candidate work.
//!
//! A read-only sweep spends its time waiting on Git subprocesses and walking
//! declared output, one candidate at a time. Candidates are independent, so a
//! dry run measures them on a few threads; a sweep that deletes, or that asks
//! owners about tasks, keeps one worker and its original order of effects.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// The most threads a read-only sweep measures candidates on.
const MAX_SCAN_WORKERS: usize = 8;

/// How many workers a sweep phase may use: one unless its candidates are
/// independent reads (`parallel_safe`), otherwise the host's parallelism up
/// to [`MAX_SCAN_WORKERS`].
pub(super) fn sweep_scan_workers(parallel_safe: bool) -> usize {
    if !parallel_safe {
        return 1;
    }
    thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(MAX_SCAN_WORKERS)
}

/// `map` over `items` on up to `workers` threads, results in item order.
///
/// Each worker builds its own `state` once and reuses it for every item it
/// takes, so one worker reproduces a sequential loop over shared state. A
/// worker that cannot be spawned leaves its items to the others; when none
/// can be spawned the items run on the calling thread. A panic in `map`
/// propagates as it would from a sequential loop.
pub(super) fn map_with_sweep_workers<T, S, R>(
    items: &[T],
    workers: usize,
    state: impl Fn() -> S + Sync,
    map: impl Fn(&S, &T) -> R + Sync,
) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let workers = workers.min(items.len());
    let sequential = || {
        let state = state();
        items.iter().map(|item| map(&state, item)).collect()
    };
    if workers <= 1 {
        return sequential();
    }
    let next = AtomicUsize::new(0);
    let (state, map, next) = (&state, &map, &next);
    let results = thread::scope(|scope| {
        let handles = (0..workers)
            .filter_map(|_| {
                thread::Builder::new()
                    .name("orbit-gc-scan".into())
                    .spawn_scoped(scope, move || {
                        let state = state();
                        let mut done = Vec::new();
                        loop {
                            let index = next.fetch_add(1, Ordering::Relaxed);
                            let Some(item) = items.get(index) else {
                                return done;
                            };
                            done.push((index, map(&state, item)));
                        }
                    })
                    .ok()
            })
            .collect::<Vec<_>>();
        if handles.is_empty() {
            return None;
        }
        Some(
            handles
                .into_iter()
                .flat_map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
                })
                .collect::<Vec<_>>(),
        )
    });
    let Some(mut results) = results else {
        return sequential();
    };
    results.sort_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}
