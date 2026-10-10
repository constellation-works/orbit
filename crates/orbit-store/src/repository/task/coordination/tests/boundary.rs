//! ORB-15107: a queued admission holds back ordinary sections that arrive
//! after it, so it is not starved by overlapping readers. That priority must
//! keep per-thread re-entrance and the host-then-partition order, and must
//! not let two exclusive waiters and nested readers wait on each other.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use orbit_common::OrbitError;
use orbit_common::fs::io::{FileLockOptions, read_file_lock_holder};
use tempfile::TempDir;

use super::super::TaskCommitBoundary;
use crate::Store;
use crate::driver::sqlite::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};

const FIRST: &str = "orbit-first-111111";
const SECOND: &str = "orbit-second-222222";

/// Long enough that only a deadlock, never ordinary scheduling, reaches it.
fn patient() -> FileLockOptions {
    FileLockOptions {
        timeout: Duration::from_secs(5),
        warn_after: Duration::from_secs(5),
        record_shared_holders: true,
        warn_held_after: None,
        prefer_exclusive_waiters: true,
    }
}

/// Two activated partitions under one host lock.
fn partitions() -> (TempDir, Arc<TaskCommitBoundary>, Arc<TaskCommitBoundary>) {
    let root = tempfile::tempdir().expect("tempdir");
    let registry =
        TaskRegistryStore::open(&task_registry_path(root.path())).expect("open registry");
    let open = |partition: &str| {
        let repo = root.path().join(partition);
        let orbit_dir = repo.join(".orbit");
        std::fs::create_dir_all(&orbit_dir).expect("create orbit dir");
        registry
            .bind_workspace(BindWorkspaceParams {
                partition_id: Some(partition.to_string()),
                slug: partition.to_string(),
                repo_root: repo.clone(),
                workspace_path: repo,
                orbit_dir,
                repo_fingerprint: None,
            })
            .expect("bind workspace");
        let store = Store::open(&root.path().join("state.sqlite")).expect("open store");
        Arc::new(
            TaskCommitBoundary::new(store, registry.clone(), partition.into())
                .expect("activate partition")
                .with_lock_options(patient()),
        )
    };
    let first = open(FIRST);
    let second = open(SECOND);
    (root, first, second)
}

/// The turnstile a writer queued on `lock_target`'s lock holds.
fn turnstile(lock_target: &Path) -> PathBuf {
    let name = lock_target
        .file_name()
        .and_then(|name| name.to_str())
        .expect("lock target name");
    lock_target.with_file_name(format!(".{name}.lock.turnstile"))
}

/// Block until an exclusive waiter has queued at `lock_target`'s turnstile.
fn wait_until_queued(lock_target: &Path) {
    let turnstile = turnstile(lock_target);
    let deadline = Instant::now() + Duration::from_secs(10);
    while read_file_lock_holder(&turnstile).is_none() {
        assert!(
            Instant::now() < deadline,
            "no writer queued at {}",
            turnstile.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Criterion: an admission acquires its partition within a bounded time while
/// ordinary sections keep overlapping, and each of those sections still
/// re-enters the boundary under its outer acquisition.
#[test]
fn an_admission_is_not_starved_by_overlapping_reentrant_ordinary_sections() {
    let (_root, boundary, _) = partitions();
    let stop = Arc::new(AtomicBool::new(false));
    let sections = Arc::new(AtomicU64::new(0));
    let readers: Vec<_> = (0..4u32)
        .map(|reader| {
            let boundary = Arc::clone(&boundary);
            let stop = Arc::clone(&stop);
            let sections = Arc::clone(&sections);
            std::thread::spawn(move || -> Result<(), OrbitError> {
                std::thread::sleep(Duration::from_millis(10) * reader);
                while !stop.load(Ordering::Relaxed) {
                    boundary.enter_ordinary(|| {
                        boundary.enter_ordinary(|| {
                            std::thread::sleep(Duration::from_millis(40));
                            Ok(())
                        })
                    })?;
                    sections.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            })
        })
        .collect();
    while sections.load(Ordering::Relaxed) < 16 {
        std::thread::sleep(Duration::from_millis(5));
    }

    let queued = Instant::now();
    let waited = boundary
        .with_admission(|| {
            // Re-entering from inside the admission runs under it.
            boundary.with_admission(|| Ok(()))?;
            boundary.enter_ordinary(|| Ok(queued.elapsed()))
        })
        .expect("the admission acquires before its deadline");
    stop.store(true, Ordering::Relaxed);
    for reader in readers {
        reader
            .join()
            .expect("reader thread")
            .expect("an ordinary section waits behind the admission, never times out");
    }
    assert!(
        waited < Duration::from_secs(2),
        "the admission waited {waited:?} behind overlapping ordinary sections"
    );
}

/// Criterion: no new deadlock. Each reader holds one partition and then reads
/// the other, in opposite orders, while an admission is queued at each
/// partition and a host-wide admission at the host lock. Had the nested reads
/// queued behind those writers, each reader would wait on a writer that waits
/// on the other reader.
#[test]
fn nested_reads_never_queue_behind_writers_they_hold_back() {
    let (_root, first, second) = partitions();
    let (inside_tx, inside_rx) = mpsc::channel();
    let reader = |outer: Arc<TaskCommitBoundary>, inner: Arc<TaskCommitBoundary>| {
        let inside = inside_tx.clone();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            outer.enter_ordinary(|| {
                inside.send(()).expect("signal inside");
                go_rx.recv().expect("go");
                inner.enter_ordinary(|| Ok(()))
            })
        });
        (go_tx, thread)
    };
    let (go_first, first_reader) = reader(Arc::clone(&first), Arc::clone(&second));
    let (go_second, second_reader) = reader(Arc::clone(&second), Arc::clone(&first));
    for _ in 0..2 {
        inside_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("reader entered");
    }

    let admit = |boundary: &Arc<TaskCommitBoundary>| {
        let boundary = Arc::clone(boundary);
        std::thread::spawn(move || boundary.with_admission(|| Ok(())))
    };
    let first_admission = admit(&first);
    wait_until_queued(&first.lock_target());
    let second_admission = admit(&second);
    wait_until_queued(&second.lock_target());
    let host_admission = {
        let boundary = Arc::clone(&first);
        std::thread::spawn(move || boundary.with_host_admission(|| Ok(())))
    };
    wait_until_queued(&first.host_lock_target());

    go_first.send(()).expect("release first reader");
    go_second.send(()).expect("release second reader");
    for (name, thread) in [
        ("first reader", first_reader),
        ("second reader", second_reader),
        ("first admission", first_admission),
        ("second admission", second_admission),
        ("host admission", host_admission),
    ] {
        thread
            .join()
            .expect("section thread")
            .unwrap_or_else(|error| panic!("{name} must finish, not time out: {error}"));
    }
}
