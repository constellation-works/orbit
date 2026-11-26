//! Bounded reads across the workspaces of a single app command.

use crate::{Core, OrbitRuntime};

/// Upper bound on the scoped threads one fleet request fans out to.
/// Each workspace read is independent (its own runtime and store), so the
/// aggregate waits for the slowest workspace instead of the sum; the bound
/// keeps a host with many registered workspaces from spawning a thread per
/// workspace per poll. Matches the fleet drains/tasks aggregates.
const MAX_FLEET_READERS: usize = 8;

pub(super) fn read<T: Send>(
    first: &Core,
    others: &[&Core],
    query: impl Fn(&OrbitRuntime) -> T + Sync,
) -> Vec<T> {
    let cores = std::iter::once(first)
        .chain(others.iter().copied())
        .collect::<Vec<_>>();
    fan_out(cores.len(), |index| query(cores[index].runtime()))
}

/// Run `work(index)` for every index in `0..count` on up to
/// [`MAX_FLEET_READERS`] scoped threads, returning the results in
/// index order. A panicking worker re-raises in the caller, exactly as the
/// serial loop would.
fn fan_out<T>(count: usize, work: impl Fn(usize) -> T + Sync) -> Vec<T>
where
    T: Send,
{
    if count <= 1 {
        return (0..count).map(work).collect();
    }
    let workers = std::thread::available_parallelism()
        .map(|parallelism| parallelism.get())
        .unwrap_or(4)
        .min(MAX_FLEET_READERS)
        .min(count);
    let chunk = count.div_ceil(workers);
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for start in (0..count).step_by(chunk) {
            let end = (start + chunk).min(count);
            let work = &work;
            handles.push(scope.spawn(move || (start..end).map(work).collect::<Vec<_>>()));
        }
        let mut results = Vec::with_capacity(count);
        for handle in handles {
            match handle.join() {
                Ok(chunk_results) => results.extend(chunk_results),
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
        results
    })
}
