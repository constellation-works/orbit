//! Process-tree sampling: interval CPU stays non-negative when a child
//! leaves, an unreadable tree writes nothing, and a real macOS tree is what
//! the minute row records.

use std::collections::VecDeque;
#[cfg(target_os = "macos")]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use orbit_common::process::ancestry::{ProcessStartKey, ProcessTreeProcess};
use orbit_store::Store;
use orbit_store::contracts::{ResourceUsageStoreBackend, RunResourceMinute};
use tempfile::TempDir;

use crate::application::job::pipeline::worker::delivery_class::DeliveryResourceClass;
use crate::application::job::pipeline::worker::usage::RunResourceSampler;

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 12, 8, 0, 0).unwrap()
}

fn at(seconds: i64) -> DateTime<Utc> {
    start() + TimeDelta::seconds(seconds)
}

fn process(pid: u32, started: u64, cpu_ns: u64, bytes: u64) -> ProcessTreeProcess {
    ProcessTreeProcess {
        key: ProcessStartKey {
            pid,
            starttime: started,
        },
        cpu_time_ns: cpu_ns,
        phys_footprint: bytes,
    }
}

struct Fixture {
    store: Arc<Store>,
    proc_root: PathBuf,
    _root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let proc_root = root.path().join("proc");
        std::fs::create_dir_all(&proc_root).unwrap();
        let store = Arc::new(Store::open(&root.path().join("orbit.db")).unwrap());
        Self {
            store,
            proc_root,
            _root: root,
        }
    }
}

fn class() -> DeliveryResourceClass {
    DeliveryResourceClass {
        task_id: "ORB-1".into(),
        work_class: "change".into(),
        complexity: Some("medium".into()),
        admitted_at: start() - TimeDelta::minutes(2),
        resumed: false,
    }
}

/// Drive `script` at `at_seconds`, then finish on the last instant. An empty
/// read stands in for the finish sample, which must not add another reading.
fn recorded(
    script: Vec<Option<Vec<ProcessTreeProcess>>>,
    at_seconds: &[i64],
) -> Vec<RunResourceMinute> {
    let fixture = Fixture::new();
    let mut queue = VecDeque::from(script);
    let mut sampler = RunResourceSampler::from_process_tree(
        Arc::clone(&fixture.store) as Arc<dyn ResourceUsageStoreBackend>,
        move || queue.pop_front().unwrap_or(None),
        fixture.proc_root.clone(),
        "ws_test".into(),
        "jrun-tree".into(),
        class(),
        start(),
    );
    let last = *at_seconds.last().unwrap();
    for seconds in at_seconds {
        sampler.sample(at(*seconds));
    }
    sampler.finish(at(last));
    assert!(
        fixture
            .store
            .host_resource_minutes(start())
            .unwrap()
            .is_empty(),
        "a process-tree sample does not invent a host minute"
    );
    fixture
        .store
        .run_resource_minutes("ws_test", "jrun-tree")
        .unwrap()
}

fn cores_of(rows: &[RunResourceMinute]) -> f64 {
    assert_eq!(rows.len(), 1, "{rows:#?}");
    rows[0].cpu_cores
}

/// A child that exits, a pid that is reused, and a counter that steps
/// backwards must not make the minute negative or replace it with a
/// lifetime total. A process that appears contributes its full CPU time.
#[test]
fn tree_intervals_charge_deltas_and_new_processes_only() {
    // 5s of wall. CPU figures below are chosen so cores land on dyadic
    // values: 1_250_000 usec / 5_000_000 usec = 0.25, and twice that is 0.5.
    let parent = |cpu_ns, bytes| process(1, 10, cpu_ns, bytes);
    let child = |cpu_ns, bytes| process(2, 20, cpu_ns, bytes);
    let reused = |cpu_ns| process(2, 21, cpu_ns, 10);

    let exited = recorded(
        vec![
            Some(vec![
                parent(10_000_000_000, 1_000),
                child(8_000_000_000, 50_000_000),
            ]),
            Some(vec![parent(11_250_000_000, 1_000)]),
        ],
        &[0, 5],
    );
    assert!(
        (cores_of(&exited) - 0.25).abs() < 1e-9,
        "the exited child's lifetime must not be subtracted or recounted: {exited:#?}"
    );
    assert_eq!(
        exited[0].memory_bytes, 50_001_000,
        "memory is the peak of the tree, including the child while it was alive"
    );

    let appeared = recorded(
        vec![
            Some(vec![parent(1_000_000_000, 100)]),
            Some(vec![
                parent(1_000_000_000, 100),
                child(2_500_000_000, 4_000),
            ]),
        ],
        &[0, 5],
    );
    assert!(
        (cores_of(&appeared) - 0.5).abs() < 1e-9,
        "a new process contributes its full CPU time: {appeared:#?}"
    );
    assert_eq!(appeared[0].memory_bytes, 4_100);

    let replaced = recorded(
        vec![
            Some(vec![child(9_000_000_000, 80)]),
            Some(vec![reused(2_500_000_000)]),
        ],
        &[0, 5],
    );
    assert!(
        (cores_of(&replaced) - 0.5).abs() < 1e-9,
        "a reused pid is a new process, not a negative delta of the old one: {replaced:#?}"
    );

    let rewound = recorded(
        vec![
            Some(vec![parent(5_000_000_000, 70)]),
            Some(vec![parent(4_000_000_000, 70)]),
        ],
        &[0, 5],
    );
    assert!(
        cores_of(&rewound).abs() < 1e-9,
        "a counter that steps backwards charges nothing: {rewound:#?}"
    );
}

/// A missed read writes no point of its own. The next success still diffs
/// against the last good snapshot, over the wall time the miss spanned.
#[test]
fn an_unreadable_tree_skips_that_sample_and_keeps_the_run() {
    let missed = recorded(vec![None, None], &[0, 5]);
    assert!(
        missed.is_empty(),
        "no readable sample, no row, and sampling returned: {missed:#?}"
    );

    let parent = |cpu_ns| process(1, 10, cpu_ns, 1_000);
    let spanned = recorded(
        vec![
            Some(vec![parent(0)]),
            None,
            Some(vec![parent(5_000_000_000)]),
        ],
        &[0, 5, 10],
    );
    assert_eq!(spanned.len(), 1, "{spanned:#?}");
    assert_eq!(spanned[0].samples, 2, "the missed instant is not a sample");
    assert!(
        (spanned[0].cpu_cores - 0.5).abs() < 1e-9,
        "CPU across the gap is the survivor's delta over the whole wall: {spanned:#?}"
    );
    assert_eq!(spanned[0].memory_bytes, 1_000);
    assert_eq!(spanned[0].task_id, "ORB-1");
    assert!(!spanned[0].resumed);
}

/// On macOS the worker's own tree, including a child and a grandchild, is the
/// minute that gets stored. A sandbox that will not let the test see that
/// tree defers the test to the host instead of accepting a smaller sample.
#[cfg(target_os = "macos")]
#[test]
fn macos_worker_records_a_spawned_child_tree() {
    const TEST: &str = "application::job::pipeline::worker::tests::usage_tree::macos_worker_records_a_spawned_child_tree";
    if defer_unreadable_tree(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let mut sampler = RunResourceSampler::from_process_tree(
        Arc::clone(&fixture.store) as Arc<dyn ResourceUsageStoreBackend>,
        || orbit_common::process::ancestry::process_tree(std::process::id()),
        fixture.proc_root.clone(),
        "ws_test".into(),
        "jrun-live".into(),
        class(),
        start(),
    );
    let before = orbit_common::process::ancestry::process_tree(std::process::id())
        .expect("the tree was readable at the start of the test");
    let before_ids: std::collections::HashSet<u32> =
        before.iter().map(|process| process.key.pid).collect();
    let before_bytes: u64 = before.iter().map(|process| process.phys_footprint).sum();
    sampler.sample(at(0));

    let hold = match spawn_child_tree(&fixture._root) {
        Ok(hold) => hold,
        Err(TreeSpawn::Unreadable) => {
            if defer_unreadable_tree(TEST) {
                return;
            }
            panic!("spawned a child tree but could not read it back, and ps was allowed");
        }
    };
    let during = orbit_common::process::ancestry::process_tree(std::process::id())
        .expect("tree readable while the child holds its allocation");
    let newcomers: Vec<_> = during
        .iter()
        .filter(|process| !before_ids.contains(&process.key.pid))
        .collect();
    assert!(
        newcomers.len() >= 2,
        "the child and its grandchild must both be in the tree: {during:#?}"
    );
    let held_identities: Vec<_> = newcomers
        .iter()
        .map(|process| {
            orbit_common::process::signal::BoundProcess::bind(process.key.pid, None)
                .expect("bind the fixture process")
                .expect("fixture is alive while its pages are held")
        })
        .collect();
    let newcomer_cpu_ns: u64 = newcomers.iter().map(|process| process.cpu_time_ns).sum();
    let during_bytes: u64 = during.iter().map(|process| process.phys_footprint).sum();
    assert!(
        during_bytes >= before_bytes.saturating_add(32 * 1024 * 1024),
        "touched anonymous pages must show in the tree footprint: before {before_bytes} during {during_bytes}"
    );

    sampler.sample(at(5));
    sampler.finish(at(5));
    drop(hold);
    for process in held_identities {
        assert!(
            !process.is_alive().expect("probe fixture cleanup"),
            "dropping the tree guard must stop the child and grandchild"
        );
    }

    let rows = fixture
        .store
        .run_resource_minutes("ws_test", "jrun-live")
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert!(
        rows[0].memory_bytes >= before_bytes.saturating_add(32 * 1024 * 1024),
        "the stored peak must include the child tree, not only the worker: {} < {before_bytes} + 32MiB (tree was {during_bytes})",
        rows[0].memory_bytes
    );
    let charged_usec = rows[0].cpu_cores * 5_000_000.0;
    let newcomer_usec = newcomer_cpu_ns as f64 / 1_000.0;
    assert!(
        charged_usec + 1.0 >= newcomer_usec,
        "cores must include the new processes' full CPU time: charged {charged_usec} usec, children {newcomer_usec} usec ({rows:#?})"
    );
    assert!(
        rows[0].cpu_cores > 0.0,
        "the child tree did CPU work between the samples: {rows:#?}"
    );
}

#[cfg(target_os = "macos")]
fn defer_unreadable_tree(test: &str) -> bool {
    if orbit_common::process::ancestry::process_tree(std::process::id()).is_some() {
        return false;
    }
    match orbit_common::test_env::ps_stat(std::process::id()) {
        orbit_common::test_env::PsRun::Denied(reason) => {
            orbit_common::test_env::report_ps_denied(test, &reason);
            true
        }
        orbit_common::test_env::PsRun::Ran(output) => {
            panic!(
                "process tree is unreadable but ps ran (status {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        }
    }
}

#[cfg(target_os = "macos")]
enum TreeSpawn {
    Unreadable,
}

#[cfg(target_os = "macos")]
type HeldTree = orbit_common::process::OwnedChild;

#[cfg(target_os = "macos")]
fn spawn_child_tree(root: &TempDir) -> Result<HeldTree, TreeSpawn> {
    const CHILD: &str = "application::job::pipeline::worker::tests::usage_tree::hold_child_tree";
    orbit_common::test_env::assert_child_test_exists(CHILD);
    let ready = root.path().join("child-ready");
    let stderr_path = root.path().join("child-stderr.txt");
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD, "--ignored"])
        .env("ORBIT_RESOURCE_TREE_ROLE", "child")
        .env("ORBIT_RESOURCE_TREE_READY", &ready)
        .env("ORBIT_RESOURCE_TREE_DIR", root.path())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr)
        .process_group(0);
    let mut child = HeldTree::from_spawned(command.spawn().unwrap());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if std::fs::read(&ready).ok().as_deref() == Some(b"ready" as &[u8]) {
            return Ok(child);
        }
        if let Some(status) = child.child_mut().try_wait().unwrap() {
            panic!(
                "child tree exited {status} before ready:\n{}",
                std::fs::read_to_string(&stderr_path).unwrap_or_default()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(child);
    if orbit_common::process::ancestry::process_tree(std::process::id()).is_none() {
        return Err(TreeSpawn::Unreadable);
    }
    panic!(
        "child tree never became ready:\n{}",
        std::fs::read_to_string(&stderr_path).unwrap_or_default()
    );
}

/// Re-executed fixture. The child allocates, starts a grandchild that
/// allocates, and both stay alive until the process group is killed.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "re-executed by macos_worker_records_a_spawned_child_tree"]
fn hold_child_tree() {
    match std::env::var("ORBIT_RESOURCE_TREE_ROLE").as_deref() {
        Ok("child") => hold_with_grandchild(),
        Ok("grandchild") => hold_leaf(16 * 1024 * 1024),
        _ => panic!("hold_child_tree is only a re-executed fixture"),
    }
}

#[cfg(target_os = "macos")]
fn hold_with_grandchild() {
    let dir = std::path::PathBuf::from(std::env::var("ORBIT_RESOURCE_TREE_DIR").unwrap());
    let ready = dir.join("grandchild-ready");
    let stderr_path = dir.join("grandchild-stderr.txt");
    let stderr = std::fs::File::create(&stderr_path).unwrap();
    const CHILD: &str = "application::job::pipeline::worker::tests::usage_tree::hold_child_tree";
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", CHILD, "--ignored"])
        .env("ORBIT_RESOURCE_TREE_ROLE", "grandchild")
        .env("ORBIT_RESOURCE_TREE_READY", &ready)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr);
    let mut grandchild = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if std::fs::read(&ready).ok().as_deref() == Some(b"ready" as &[u8]) {
            // Does not return. The parent's process-group kill ends both
            // processes while their allocations are still mapped.
            hold_leaf(32 * 1024 * 1024);
        }
        if grandchild.try_wait().ok().flatten().is_some() {
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = grandchild.kill();
    let _ = grandchild.wait();
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn hold_leaf(bytes: usize) {
    let mut pages = vec![0u8; bytes];
    for offset in (0..bytes).step_by(4096) {
        pages[offset] = 1;
    }
    let mut state = 0x1234_5678_9abc_def0u64;
    for step in 0..4_000_000u64 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(step)
            ^ u64::from(pages[(step as usize) % bytes]);
    }
    std::hint::black_box(state);
    let ready = std::path::PathBuf::from(std::env::var("ORBIT_RESOURCE_TREE_READY").unwrap());
    std::fs::write(&ready, b"ready").unwrap();
    // Stay until the parent kills the group. The allocation has to remain
    // mapped for the sampler's second read.
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
        std::hint::black_box(pages.len());
    }
}
