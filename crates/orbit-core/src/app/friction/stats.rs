//! `stats`: compute friction rates.

use orbit_common::OrbitError;
use serde_json::Value;

use crate::OrbitRuntime;

pub(super) fn stats(runtime: &OrbitRuntime) -> Result<Value, OrbitError> {
    // Rates read only status and attribution, so envelope metadata suffices.
    // Owner reads are observational. A task commit publishes the decided
    // envelope before it updates the index row, and holds that task's bundle
    // lock until deferred apply finishes. The operational metadata read waits
    // on that lock (`index writer wait`, up to two seconds) and, when the
    // wait expires with the same row still stale, scans and rebuilds the
    // workspace index. Dashboard list and stats polls hit that window on
    // every in-flight commit. A worker still reads through its owner.
    let tasks = if runtime.worker_invocation().is_some() {
        runtime.list_task_metadata()?
    } else {
        runtime.observe_task_metadata()?
    };
    crate::runtime::stored::friction::store_for(runtime)?.stats(&tasks)
}
