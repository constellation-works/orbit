//! Concurrent and repeated stall reports share one friction per divergence.
//!
//! The miss hook is the deterministic interleaving seam: both reporters pass
//! the preliminary lookup, and the store is still empty, before either insert.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use orbit_automation::delivery::stall::StallReport;
use orbit_store::Store;
use orbit_store::friction_store::{FrictionListFilter, FrictionStore};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::automation::recovery::HistoryDivergence;

use super::super::stall::{self, dedupe_miss_hook};
use crate::OrbitRuntime;

fn serialize_reports() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct ClearHook;

impl Drop for ClearHook {
    fn drop(&mut self) {
        dedupe_miss_hook::clear();
    }
}

struct Gate {
    arrived: usize,
    scan_done: bool,
}

fn runtime() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir().expect("tempdir");
    let global_root = root.path().join("global");
    let workspace_root = root.path().join("repo").join(".orbit");
    std::fs::create_dir_all(&global_root).expect("global root");
    std::fs::create_dir_all(&workspace_root).expect("workspace root");
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).expect("runtime");
    (root, runtime)
}

fn report(
    runtime: &OrbitRuntime,
    consumer: &str,
    commit: &str,
    at: chrono::DateTime<Utc>,
) -> String {
    let divergence = HistoryDivergence {
        observed: SourceRevision {
            commit: commit.to_string(),
            tree: format!("tree-{commit}"),
        },
        head: SourceRevision {
            commit: "head".to_string(),
            tree: "tree-head".to_string(),
        },
        refusal: "replay refused".to_string(),
        obligations: vec!["delivery d1 (evidence)".to_string()],
    };
    stall::report(
        runtime,
        &StallReport {
            consumer,
            repository: "repo",
            branch: "agent-main",
            reason: "history_diverged",
            divergence: Some(&divergence),
            repaired: false,
            at,
        },
    )
    .expect("report stall")
    .expect("stall report returns the friction id")
}

fn list_all(store: &Store, workspace_id: &str, files_root: &std::path::Path) -> Vec<String> {
    FrictionStore::open(store.clone(), workspace_id, files_root)
        .expect("open friction store")
        .list(&FrictionListFilter::default())
        .expect("list frictions")
        .into_iter()
        .map(|row| row.record.id)
        .collect()
}

#[test]
fn concurrent_reporters_for_one_divergence_persist_one_friction() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &concurrent_reporters_for_one_divergence_persist_one_friction,
    )) {
        return;
    }
    let _serialize = serialize_reports();
    let (_root, runtime) = runtime();
    let store = runtime.sqlite_store().expect("store");
    let workspace_id = runtime.workspace_id().expect("workspace");
    let files_root = runtime.data_root().join("frictions");
    let saw_empty = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((
        Mutex::new(Gate {
            arrived: 0,
            scan_done: false,
        }),
        Condvar::new(),
    ));

    let saw = Arc::clone(&saw_empty);
    let gate_hook = Arc::clone(&gate);
    let listed_store = store.clone();
    let listed_workspace = workspace_id.clone();
    let listed_root = files_root.clone();
    dedupe_miss_hook::install(move || {
        let (lock, cv) = &*gate_hook;
        let mut gate = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gate.arrived += 1;
        if gate.arrived == 1 {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !gate.scan_done {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    panic!("peer reporter did not pass the dedupe lookup");
                }
                let (guard, waited) = cv
                    .wait_timeout(gate, remaining)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                gate = guard;
                if waited.timed_out() && !gate.scan_done {
                    panic!("peer reporter did not pass the dedupe lookup");
                }
            }
        } else {
            drop(gate);
            let rows = list_all(&listed_store, &listed_workspace, &listed_root);
            assert!(
                rows.is_empty(),
                "ORB-13928: both reporters passed the lookup before either insert"
            );
            saw.store(true, Ordering::SeqCst);
            let mut gate = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            gate.scan_done = true;
            cv.notify_all();
        }
    });
    let _clear = ClearHook;

    let at = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
    let runtime_a = runtime.clone();
    let runtime_b = runtime.clone();
    let (id_a, id_b) = std::thread::scope(|scope| {
        let alpha = scope.spawn(|| report(&runtime_a, "host/ws/auto-task/alpha", "commit-a", at));
        let beta = scope.spawn(|| report(&runtime_b, "host/ws/auto-task/beta", "commit-a", at));
        (alpha.join().expect("alpha"), beta.join().expect("beta"))
    });

    assert!(
        saw_empty.load(Ordering::SeqCst),
        "ORB-13928: the race window must be forced, not left to timing"
    );
    assert_eq!(
        id_a, id_b,
        "ORB-13928: concurrent reports for one divergence return one friction id"
    );
    let ids = list_all(&store, &workspace_id, &files_root);
    assert_eq!(
        ids,
        vec![id_a],
        "ORB-13928: one persisted friction for one divergence"
    );
}

#[test]
fn retries_and_sibling_consumers_reuse_one_divergence_record() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &retries_and_sibling_consumers_reuse_one_divergence_record,
    )) {
        return;
    }
    let _serialize = serialize_reports();
    let (_root, runtime) = runtime();
    let at = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();

    let first = report(&runtime, "host/ws/auto-task/alpha", "commit-a", at);
    let retry = report(&runtime, "host/ws/auto-task/alpha", "commit-a", at);
    let sibling = report(&runtime, "host/ws/auto-task/beta", "commit-a", at);
    let distinct = report(&runtime, "host/ws/auto-task/alpha", "commit-b", at);

    assert_eq!(first, retry, "a retry reuses the divergence record");
    assert_eq!(
        first, sibling,
        "a sibling consumer reuses the divergence record"
    );
    assert_ne!(
        first, distinct,
        "a different divergence gets its own record"
    );

    let ids = list_all(
        &runtime.sqlite_store().expect("store"),
        &runtime.workspace_id().expect("workspace"),
        &runtime.data_root().join("frictions"),
    );
    assert_eq!(ids.len(), 2, "two divergences persist two frictions");
    assert!(ids.contains(&first));
    assert!(ids.contains(&distinct));
}
