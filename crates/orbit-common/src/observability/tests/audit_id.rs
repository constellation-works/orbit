use crate::observability::audit_id::audit_execution_id;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

#[test]
fn audit_execution_id_is_unique_under_concurrent_generation() {
    let workers = 16;
    let per_worker = 64;
    let barrier = Arc::new(Barrier::new(workers));

    let handles: Vec<_> = (0..workers)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                (0..per_worker)
                    .map(|_| audit_execution_id("exec"))
                    .collect::<Vec<_>>()
            })
        })
        .collect();

    let ids: Vec<String> = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("worker thread joined"))
        .collect();
    let unique: BTreeSet<_> = ids.iter().cloned().collect();

    assert_eq!(ids.len(), workers * per_worker);
    assert_eq!(unique.len(), ids.len());
    assert!(ids.iter().all(|id| id.starts_with("exec-")));
}
