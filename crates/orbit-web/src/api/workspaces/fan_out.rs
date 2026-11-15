//! Bounded parallel reads across every registered workspace.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Upper bound on the scoped threads one aggregate request fans out to.
/// Each workspace read is independent (its own runtime and store), so the
/// aggregate waits for the slowest workspace instead of the sum; the bound
/// keeps a host with many registered workspaces from spawning a thread per
/// workspace per poll.
const MAX_AGGREGATE_FAN_OUT: usize = 8;

/// Run `work(index)` for every index in `0..count` on up to
/// [`MAX_AGGREGATE_FAN_OUT`] scoped threads, returning the results in
/// index order. Each thread takes the next unclaimed index, so one slow
/// workspace delays no other. A panicking worker re-raises in the caller,
/// exactly as the serial loop would.
pub(super) fn fan_out<T>(count: usize, work: impl Fn(usize) -> T + Sync) -> Vec<T>
where
    T: Send,
{
    if count <= 1 {
        return (0..count).map(work).collect();
    }
    let workers = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4)
        .min(MAX_AGGREGATE_FAN_OUT)
        .min(count);
    let next_index = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let handles = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    let mut claimed = Vec::new();
                    loop {
                        let index = next_index.fetch_add(1, Ordering::Relaxed);
                        if index >= count {
                            return claimed;
                        }
                        claimed.push((index, work(index)));
                    }
                })
            })
            .collect::<Vec<_>>();
        let mut results = Vec::with_capacity(count);
        for handle in handles {
            match handle.join() {
                Ok(claimed) => results.extend(claimed),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        results.sort_unstable_by_key(|(index, _)| *index);
        results.into_iter().map(|(_, result)| result).collect()
    })
}
