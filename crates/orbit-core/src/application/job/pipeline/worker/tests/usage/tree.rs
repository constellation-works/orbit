//! Process-tree sampling: interval CPU stays non-negative when a child
//! leaves, a reaped descendant is charged once, a descendant reaped outside
//! the tree does not hide later work, an unreadable tree writes nothing, a
//! real macOS tree is what the minute row records, and, run explicitly, how
//! much of a build-like workload's CPU the worker's tree captures.

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
        parent: None,
        cpu_time_ns: cpu_ns,
        reaped_cpu_time_ns: 0,
        phys_footprint: bytes,
    }
}

fn reaped(process: ProcessTreeProcess, reaped_ns: u64) -> ProcessTreeProcess {
    ProcessTreeProcess {
        reaped_cpu_time_ns: reaped_ns,
        ..process
    }
}

fn under(process: ProcessTreeProcess, parent: &ProcessTreeProcess) -> ProcessTreeProcess {
    ProcessTreeProcess {
        parent: Some(parent.key),
        ..process
    }
}

/// Where a process that left the tree went: `(pid, start, parent pid)` for
/// each one still alive. Every other pid reads as exited.
fn alive(processes: &[(u32, u64, u32)]) -> impl FnMut(u32) -> Option<(ProcessStartKey, u32)> {
    let processes = processes.to_vec();
    move |pid| {
        processes
            .iter()
            .find(|(alive, _, _)| *alive == pid)
            .map(|(pid, starttime, ppid)| {
                (
                    ProcessStartKey {
                        pid: *pid,
                        starttime: *starttime,
                    },
                    *ppid,
                )
            })
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
    recorded_with(script, at_seconds, alive(&[]))
}

/// [`recorded`] where `locate` says where each process that left went.
fn recorded_with(
    script: Vec<Option<Vec<ProcessTreeProcess>>>,
    at_seconds: &[i64],
    locate: impl FnMut(u32) -> Option<(ProcessStartKey, u32)> + Send + 'static,
) -> Vec<RunResourceMinute> {
    let fixture = Fixture::new();
    let mut queue = VecDeque::from(script);
    let mut sampler = RunResourceSampler::from_process_tree(
        Arc::clone(&fixture.store) as Arc<dyn ResourceUsageStoreBackend>,
        move || queue.pop_front().unwrap_or(None),
        locate,
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
/// lifetime total. A process that appears contributes its full CPU time,
/// and a descendant reaped between samples is charged through its parent.
#[test]
fn tree_intervals_charge_deltas_and_new_processes_only() {
    // 5s of wall. CPU figures below are chosen so cores land on dyadic
    // values: 1_250_000 usec / 5_000_000 usec = 0.25, and twice that is 0.5.
    let parent = |cpu_ns, bytes| process(1, 10, cpu_ns, bytes);
    let child = |cpu_ns, bytes| under(process(2, 20, cpu_ns, bytes), &parent(0, 0));
    let reused = |cpu_ns| under(process(2, 21, cpu_ns, 10), &parent(0, 0));

    // The child ran 0.5 s more after the first sample, then the parent
    // (0.75 s of its own) reaped it: only that tail and the parent's growth
    // are new.
    let exited = recorded(
        vec![
            Some(vec![
                parent(10_000_000_000, 1_000),
                child(8_000_000_000, 50_000_000),
            ]),
            Some(vec![reaped(parent(10_750_000_000, 1_000), 8_500_000_000)]),
        ],
        &[0, 5],
    );
    assert!(
        (cores_of(&exited) - 0.25).abs() < 1e-9,
        "the reaped child's sampled lifetime must not be charged again: {exited:#?}"
    );
    assert_eq!(
        exited[0].memory_bytes, 50_001_000,
        "memory is the peak of the tree, including the child while it was alive"
    );

    let unseen = recorded(
        vec![
            Some(vec![parent(1_000_000_000, 100)]),
            Some(vec![reaped(parent(1_000_000_000, 100), 1_250_000_000)]),
        ],
        &[0, 5],
    );
    assert!(
        (cores_of(&unseen) - 0.25).abs() < 1e-9,
        "a descendant that ran and was reaped between samples counts through its parent: {unseen:#?}"
    );

    // The walk read the parent before the child was reaped, then missed the
    // child: its reap lands one sample later and is still charged once.
    let late_reap = recorded(
        vec![
            Some(vec![parent(1_000_000_000, 100), child(4_000_000_000, 10)]),
            Some(vec![parent(1_000_000_000, 100)]),
            Some(vec![reaped(parent(1_000_000_000, 100), 6_500_000_000)]),
        ],
        &[0, 5, 10],
    );
    assert!(
        (cores_of(&late_reap) - 0.25).abs() < 1e-9,
        "a reap seen one sample after the child left is its 2.5 s tail: {late_reap:#?}"
    );

    // A child reaped by a grandchild-reaping parent that the root then
    // reaps, each reap landing a sample after the departure, owes its whole
    // subtree to the root until the root's reaped total absorbs it. Later
    // growth is charged in full.
    let grandchild = |cpu_ns| under(process(3, 30, cpu_ns, 10), &child(0, 0));
    let nested = recorded(
        vec![
            Some(vec![
                parent(0, 100),
                child(1_000_000_000, 10),
                grandchild(2_000_000_000),
            ]),
            Some(vec![parent(0, 100), child(1_000_000_000, 10)]),
            Some(vec![parent(0, 100)]),
            Some(vec![reaped(parent(0, 100), 3_000_000_000)]),
            Some(vec![reaped(parent(5_000_000_000, 100), 3_000_000_000)]),
        ],
        &[0, 5, 10, 15, 20],
    );
    assert!(
        (cores_of(&nested) - 0.25).abs() < 1e-9,
        "nested reaps across samples charge only the root's 5 s of new work: {nested:#?}"
    );

    // A killed build: the shell under the root dies and the root reaps it,
    // but the build the shell never waited for is reaped outside the tree.
    // Its debt climbs through the shell to the root and lapses after the
    // root's next sighting, whether the shell's reap lands in the snapshot
    // it left or the next one. The root's later short-lived commands are
    // charged in full instead of paying a debt nothing will absorb.
    let shell = |cpu_ns| under(process(2, 20, cpu_ns, 10), &parent(0, 0));
    let build = |cpu_ns| under(process(3, 30, cpu_ns, 10), &shell(0));
    for shell_reap_seen_late in [false, true] {
        let shell_reap_seen = if shell_reap_seen_late { 0 } else { 500_000_000 };
        let killed = recorded(
            vec![
                Some(vec![
                    parent(0, 100),
                    shell(500_000_000),
                    build(100_000_000_000),
                ]),
                Some(vec![reaped(parent(0, 100), shell_reap_seen)]),
                Some(vec![reaped(parent(0, 100), 500_000_000)]),
                Some(vec![reaped(parent(0, 100), 3_000_000_000)]),
                Some(vec![reaped(parent(0, 100), 5_500_000_000)]),
            ],
            &[0, 5, 10, 15, 20],
        );
        assert!(
            (cores_of(&killed) - 0.25).abs() < 1e-9,
            "an orphaned build's debt must not absorb the root's later 5 s of reaped commands (shell reap late: {shell_reap_seen_late}): {killed:#?}"
        );
    }

    // A child that is reparented away was charged while it ran here and
    // will be reaped elsewhere, so it must not offset the parent's growth.
    let reparented = recorded_with(
        vec![
            Some(vec![parent(1_000_000_000, 100), child(4_000_000_000, 10)]),
            Some(vec![parent(1_000_000_000, 100)]),
            Some(vec![parent(3_500_000_000, 100)]),
            Some(vec![parent(6_000_000_000, 100)]),
        ],
        &[0, 5, 10, 15],
        alive(&[(2, 20, 4_242)]),
    );
    assert!(
        (cores_of(&reparented) - 5.0 / 15.0).abs() < 1e-9,
        "a reparented child owes nothing: {reparented:#?}"
    );

    // A child the walk missed while it was still the parent's is neither
    // written off nor charged again when it is seen once more.
    let missed = recorded_with(
        vec![
            Some(vec![parent(1_000_000_000, 100), child(4_000_000_000, 10)]),
            Some(vec![parent(1_000_000_000, 100)]),
            Some(vec![parent(1_000_000_000, 100), child(6_500_000_000, 10)]),
        ],
        &[0, 5, 10],
        alive(&[(2, 20, 1)]),
    );
    assert!(
        (cores_of(&missed) - 0.25).abs() < 1e-9,
        "a missed child is charged only its 2.5 s of growth: {missed:#?}"
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

    // The parent reaped the old child before its pid was reused.
    let replaced = recorded(
        vec![
            Some(vec![parent(1_000_000_000, 0), child(9_000_000_000, 80)]),
            Some(vec![
                reaped(parent(1_000_000_000, 0), 9_000_000_000),
                reused(2_500_000_000),
            ]),
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
    const TEST: &str = "application::job::pipeline::worker::tests::usage::tree::macos_worker_records_a_spawned_child_tree";
    if defer_unreadable_tree(TEST) {
        return;
    }

    let fixture = Fixture::new();
    let mut sampler = RunResourceSampler::from_process_tree(
        Arc::clone(&fixture.store) as Arc<dyn ResourceUsageStoreBackend>,
        || orbit_common::process::ancestry::process_tree(std::process::id()),
        orbit_common::process::ancestry::process_start_and_parent,
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
    // Bind the fixture's own processes, not other tests' children that may
    // share this tree, so teardown can prove those identities are gone.
    let leader = hold.pid();
    let held_identities: Vec<_> = newcomers
        .iter()
        .map(|process| process.key.pid)
        .filter(|pid| {
            *pid == leader
                || orbit_common::process::ancestry::process_start_and_parent(*pid)
                    .is_some_and(|(_, parent)| parent == leader)
        })
        .map(|pid| {
            orbit_common::process::signal::BoundProcess::bind(pid, None)
                .expect("bind the fixture process")
                .expect("fixture is alive while its pages are held")
        })
        .collect();
    assert_eq!(
        held_identities.len(),
        2,
        "the fixture's child and grandchild are both in the tree: {during:#?}"
    );
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
    assert_eq!(
        rows[0].cpu_accounting.as_deref(),
        Some(orbit_store::contracts::CPU_ACCOUNTING_MACOS),
        "native worker minutes carry reaped-descendant Mach accounting provenance"
    );
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
fn spawn_child_tree(root: &TempDir) -> Result<orbit_common::process::OwnedChild, TreeSpawn> {
    const CHILD: &str = "application::job::pipeline::worker::tests::usage::tree::hold_child_tree";
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
    // Own the group from spawn, so a panic or timeout before readiness still
    // kills the child and its grandchild.
    let mut child = orbit_common::process::OwnedChild::from_spawned(command.spawn().unwrap());
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
    const CHILD: &str = "application::job::pipeline::worker::tests::usage::tree::hold_child_tree";
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

/// Native attribution, run explicitly on a Mac:
/// `scripts/build-budget.py -- cargo test -p orbit-core --lib
/// application::job::pipeline::worker::tests::usage::tree::native_tree_attributes_a_build_like_workload
/// -- --ignored --exact --nocapture`.
///
/// A re-executed worker leads its own session, as a pipeline worker does, and
/// samples itself every [`RESOURCE_SAMPLE_INTERVAL`] while a build-like
/// workload runs under it: a driver keeps three wrapper-plus-compiler jobs
/// busy, a short-lived client starts a compile server that outlives it, and a
/// shell exits under a build it never waited for. Every workload process
/// reports its own `getrusage` CPU when it exits, which is the reference. The
/// JSON line compares it with the session tree the worker records, with the
/// parent-link walk alone, and with host-wide non-idle CPU over the same
/// window, which on a shared host includes other work.
///
/// [`RESOURCE_SAMPLE_INTERVAL`]: crate::runtime::host_resource::RESOURCE_SAMPLE_INTERVAL
#[cfg(target_os = "macos")]
#[test]
#[ignore = "native macOS attribution measurement; run explicitly in isolation"]
fn native_tree_attributes_a_build_like_workload() {
    use attribution::*;

    if let Ok(role) = std::env::var(ROLE) {
        run_role(&role);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut worker = role_command("worker", dir.path());
    // Safety: `setsid` is async-signal-safe and touches only the child.
    unsafe {
        worker.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    worker
        .arg("--nocapture")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = worker.output().expect("run the attribution worker");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "attribution worker {}:\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = stdout
        .lines()
        .find_map(|line| serde_json::from_str(line).ok())
        .unwrap_or_else(|| panic!("no attribution JSON in worker output:\n{stdout}"));
    std::io::Write::write_all(&mut std::io::stdout(), format!("{report}\n").as_bytes()).unwrap();
    let workload = report["workload_cpu_s"]["total"].as_f64().unwrap();
    let recorded = report["tree_cpu_s"]["session_tree"].as_f64().unwrap();
    assert!(
        workload >= 10.0,
        "the workload must do enough CPU work to measure: {report:#}"
    );
    assert!(
        recorded >= 0.85 * workload,
        "the worker's tree must capture at least 85% of its workload's CPU: {report:#}"
    );
    assert!(
        recorded <= 1.1 * workload,
        "the worker's tree must not adopt CPU that is not its workload's: {report:#}"
    );
}

#[cfg(target_os = "macos")]
mod attribution {
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use orbit_common::process::ancestry::{process_start_and_parent, process_tree};

    use crate::application::job::pipeline::worker::usage::tree::{
        TreeSampler, session_tree_reader,
    };
    use crate::runtime::host_resource::{RESOURCE_SAMPLE_INTERVAL, read_host_usage};

    pub(super) const ROLE: &str = "ORBIT_TREE_ATTRIBUTION_ROLE";
    const DIR: &str = "ORBIT_TREE_ATTRIBUTION_DIR";
    const TEST: &str = "application::job::pipeline::worker::tests::usage::tree::native_tree_attributes_a_build_like_workload";
    const JOBS: usize = 3;
    const COMPILES: usize = 16;
    const COMPILE_CPU: Duration = Duration::from_millis(750);
    const CLIENT_CPU: Duration = Duration::from_millis(200);
    const SERVER_REQUESTS: u32 = 12;
    const SERVER_REQUEST_CPU: Duration = Duration::from_millis(400);
    const ORPHAN_CPU: Duration = Duration::from_secs(2);
    /// Driver, each wrapper and compiler, client and shell.
    const BUILD_PROCESSES: usize = 3 + 2 * COMPILES;
    const DEADLINE: Duration = Duration::from_secs(120);

    pub(super) fn role_command(role: &str, dir: &Path) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", TEST, "--ignored", "--quiet"])
            .env(ROLE, role)
            .env(DIR, dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn spawn(role: &str) -> Child {
        role_command(role, &dir())
            .spawn()
            .unwrap_or_else(|error| panic!("spawn {role}: {error}"))
    }

    fn dir() -> PathBuf {
        PathBuf::from(std::env::var(DIR).unwrap())
    }

    fn own_cpu_ns() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // Safety: the pointer names a writable rusage structure.
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        // Safety: the successful call initialized the structure.
        let usage = unsafe { usage.assume_init() };
        let usec = (usage.ru_utime.tv_sec + usage.ru_stime.tv_sec) as u64 * 1_000_000
            + (usage.ru_utime.tv_usec + usage.ru_stime.tv_usec) as u64;
        usec * 1_000
    }

    /// Spend `cpu` of this process's own CPU time, however loaded the host.
    fn burn(cpu: Duration) {
        let target = own_cpu_ns() + cpu.as_nanos() as u64;
        let mut value = 1_u64;
        while own_cpu_ns() < target {
            for _ in 0..10_000 {
                value =
                    std::hint::black_box(value.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
        }
    }

    /// Record this process's lifetime CPU under `component`, atomically, as
    /// the last thing it does.
    fn report(component: &str) {
        let dir = dir();
        let pid = std::process::id();
        let pending = dir.join(format!(".{component}.{pid}"));
        std::fs::write(&pending, own_cpu_ns().to_string()).unwrap();
        std::fs::rename(&pending, dir.join(format!("{component}.{pid}"))).unwrap();
    }

    fn reports() -> Vec<(String, u64)> {
        std::fs::read_dir(dir())
            .unwrap()
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                let (component, _pid) = name.split_once('.').filter(|_| !name.starts_with('.'))?;
                let ns = std::fs::read_to_string(dir().join(&name))
                    .ok()?
                    .parse()
                    .ok()?;
                Some((component.to_string(), ns))
            })
            .collect()
    }

    fn samples_taken() -> u64 {
        std::fs::read_to_string(dir().join("samples"))
            .ok()
            .and_then(|count| count.parse().ok())
            .unwrap_or(0)
    }

    // The client and the shell exit without waiting for a child on purpose:
    // launchd adopts and reaps it, which is what is being measured.
    #[allow(clippy::zombie_processes)]
    pub(super) fn run_role(role: &str) {
        match role {
            "worker" => worker(),
            "driver" => {
                let mut others = vec![spawn("client"), spawn("shell")];
                let mut running: Vec<Child> = Vec::new();
                for _ in 0..COMPILES {
                    while running.len() >= JOBS {
                        let before = running.len();
                        running.retain_mut(|job| job.try_wait().unwrap().is_none());
                        if running.len() == before {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                    }
                    running.push(spawn("wrapper"));
                }
                for mut child in running.into_iter().chain(others.drain(..)) {
                    assert!(child.wait().unwrap().success());
                }
                report("build");
            }
            "wrapper" => {
                assert!(spawn("compiler").wait().unwrap().success());
                report("build");
            }
            "compiler" => {
                burn(COMPILE_CPU);
                report("build");
            }
            "client" => {
                // Starts the server and exits without waiting for it, so
                // launchd adopts the server.
                let _server = spawn("server");
                burn(CLIENT_CPU);
                report("build");
            }
            "server" => {
                for _ in 0..SERVER_REQUESTS {
                    burn(SERVER_REQUEST_CPU);
                    std::thread::sleep(Duration::from_millis(100));
                }
                // A compile server idles until its timeout before exiting.
                // Waiting out one more sample stands in for that idle tail.
                let served = samples_taken();
                let idle_until = Instant::now() + Duration::from_secs(30);
                while samples_taken() <= served && Instant::now() < idle_until {
                    std::thread::sleep(Duration::from_millis(50));
                }
                report("server");
            }
            "shell" => {
                // A shell that dies under a build it started: the build keeps
                // running, adopted by launchd.
                let _build = spawn("orphan");
                report("build");
            }
            "orphan" => {
                burn(ORPHAN_CPU);
                report("orphan");
            }
            other => panic!("unknown attribution role {other}"),
        }
    }

    fn worker() {
        let root = std::process::id();
        let mut session = TreeSampler::new(session_tree_reader(root), process_start_and_parent);
        let mut parent_links =
            TreeSampler::new(move || process_tree(root), process_start_and_parent);
        let sample = |session: &mut TreeSampler, parent_links: &mut TreeSampler| {
            let recorded = session.usage().expect("the session tree is readable");
            let walked = parent_links.usage().expect("the process tree is readable");
            (recorded.cpu_usage_usec, walked.cpu_usage_usec)
        };
        let proc_root = PathBuf::from("/proc");
        let host_before = read_host_usage(&proc_root).expect("host CPU is readable");
        let own_before = own_cpu_ns();
        let started = Instant::now();
        sample(&mut session, &mut parent_links);
        std::fs::write(dir().join("samples"), "0").unwrap();

        let expected = BUILD_PROCESSES + 2;
        let mut driver = spawn("driver");
        let mut driver_done = false;
        let mut samples = 0_u64;
        let mut next_sample = started + RESOURCE_SAMPLE_INTERVAL;
        let (session_usec, walked_usec) = loop {
            std::thread::sleep(Duration::from_millis(50));
            if !driver_done && let Some(status) = driver.try_wait().unwrap() {
                assert!(status.success(), "driver {status}");
                driver_done = true;
            }
            let finished = driver_done && reports().len() >= expected;
            if finished || Instant::now() >= next_sample {
                let taken = sample(&mut session, &mut parent_links);
                samples += 1;
                std::fs::write(dir().join("samples"), samples.to_string()).unwrap();
                next_sample += RESOURCE_SAMPLE_INTERVAL;
                if finished {
                    break taken;
                }
            }
            assert!(
                started.elapsed() < DEADLINE,
                "workload did not finish: {:?}",
                reports()
            );
        };
        let wall = started.elapsed().as_secs_f64();
        let own = own_cpu_ns() - own_before;
        let host_after = read_host_usage(&proc_root).expect("host CPU is readable");
        let (busy, total) = host_after
            .cpu_delta(host_before)
            .expect("host ticks advanced");
        let system = busy as f64 / total as f64 * f64::from(host_after.cpu_count) * wall;

        let reports = reports();
        let component = |name: &str| {
            reports
                .iter()
                .filter(|(component, _)| component == name)
                .map(|(_, ns)| *ns as f64 / 1e9)
                .sum::<f64>()
        };
        let workload = component("build") + component("server") + component("orphan");
        // The worker's own sampling is in the tree but not in the workload.
        let own_s = own as f64 / 1e9;
        let session_s = session_usec as f64 / 1e6 - own_s;
        let walked_s = walked_usec as f64 / 1e6 - own_s;
        let report = serde_json::json!({
            "schema_version": 1,
            "sample_interval_s": RESOURCE_SAMPLE_INTERVAL.as_secs_f64(),
            "samples": samples,
            "wall_s": wall,
            "workload_processes": reports.len(),
            "workload_cpu_s": {
                "total": workload,
                "build": component("build"),
                "server": component("server"),
                "orphan": component("orphan"),
            },
            "worker_own_cpu_s": own_s,
            "tree_cpu_s": { "session_tree": session_s, "parent_links": walked_s },
            "attribution": {
                "session_tree": session_s / workload,
                "parent_links": walked_s / workload,
            },
            "system_nonidle_cpu_s": system,
            "system_cpu_count": host_after.cpu_count,
            "system_nonidle_outside_session_tree_s": system - session_s - own_s,
        });
        std::io::Write::write_all(&mut std::io::stdout(), format!("{report}\n").as_bytes())
            .unwrap();
    }
}
