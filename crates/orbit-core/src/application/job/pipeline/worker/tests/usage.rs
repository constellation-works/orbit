//! The per-minute resource sampler, driven against fixture cgroup and `/proc`
//! directories on an injected clock, into a real store. Deterministic: every
//! counter is written by the test, nothing generates load.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use orbit_store::Store;
use orbit_store::contracts::{HostResourceMinute, ResourceUsageStoreBackend};
use tempfile::TempDir;

use crate::application::job::pipeline::worker::delivery_class::DeliveryResourceClass;
use crate::application::job::pipeline::worker::scope::WorkerScopeCgroup;
use crate::application::job::pipeline::worker::usage::RunResourceSampler;

const GB: u64 = 1_000_000_000;
const CPUS: u64 = 4;
/// `USER_HZ`: `/proc/stat` ticks per CPU per second.
const HZ: u64 = 100;

fn start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 12, 8, 0, 0).unwrap()
}

struct Fixture {
    root: TempDir,
    database: PathBuf,
    store: Arc<Store>,
    proc_root: PathBuf,
    /// Cumulative fixture counters.
    usage_usec: u64,
    busy_ticks: u64,
    idle_ticks: u64,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("orbit.db");
        let store = Arc::new(Store::open(&database).unwrap());
        let proc_root = root.path().join("proc");
        std::fs::create_dir_all(&proc_root).unwrap();
        Self {
            root,
            database,
            store,
            proc_root,
            usage_usec: 0,
            busy_ticks: 0,
            idle_ticks: 0,
        }
    }

    /// A worker scope directory for `run_id`, located the way a worker
    /// locates its own from `/proc/self/cgroup`.
    fn scope(&self, run_id: &str) -> WorkerScopeCgroup {
        let cgroup_root = self.root.path().join("cgroup");
        let path = format!(
            "user.slice/user-1000.slice/user@1000.service/app.slice/orbit-worker-{run_id}-0000abcd.scope"
        );
        std::fs::create_dir_all(cgroup_root.join(&path)).unwrap();
        WorkerScopeCgroup::from_proc_cgroup(&format!("0::/{path}\n"), &cgroup_root).unwrap()
    }

    fn scope_dir(&self, run_id: &str) -> PathBuf {
        self.root.path().join(format!(
            "cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/orbit-worker-{run_id}-0000abcd.scope"
        ))
    }

    fn sampler(&self, run_id: &str) -> RunResourceSampler {
        RunResourceSampler::new(
            Arc::clone(&self.store) as Arc<dyn ResourceUsageStoreBackend>,
            self.scope(run_id),
            self.proc_root.clone(),
            "ws_test".into(),
            run_id.into(),
            DeliveryResourceClass {
                task_id: "ORB-1".into(),
                work_class: "change".into(),
                complexity: Some("hard".into()),
                admitted_at: start() - TimeDelta::minutes(3),
                resumed: false,
            },
            start(),
        )
    }

    fn write_scope(&self, run_id: &str, usage_usec: u64, current: u64, file: u64, shmem: u64) {
        let dir = self.scope_dir(run_id);
        std::fs::write(
            dir.join("cpu.stat"),
            format!("usage_usec {usage_usec}\nuser_usec 0\nsystem_usec 0\n"),
        )
        .unwrap();
        std::fs::write(dir.join("memory.current"), format!("{current}\n")).unwrap();
        std::fs::write(
            dir.join("memory.stat"),
            format!(
                "anon {}\nfile {file}\nkernel 0\nshmem {shmem}\nfile_mapped 0\n",
                current.saturating_sub(file)
            ),
        )
        .unwrap();
    }

    /// Advance the scope by `seconds` at `cores`, and set its memory.
    fn run_scope(&mut self, run_id: &str, seconds: f64, cores: f64, current: u64, file: u64) {
        self.usage_usec += (seconds * cores * 1_000_000.0) as u64;
        self.write_scope(run_id, self.usage_usec, current, file, 0);
    }

    /// Advance the host by `seconds` with `busy` of its cores busy.
    fn run_host(&mut self, seconds: u64, busy: u64, used_kb: u64) {
        self.busy_ticks += busy * HZ * seconds;
        self.idle_ticks += (CPUS - busy) * HZ * seconds;
        let mut stat = format!(
            "cpu  {} 0 0 {} 0 0 0 0 0 0\n",
            self.busy_ticks, self.idle_ticks
        );
        for cpu in 0..CPUS {
            stat.push_str(&format!("cpu{cpu} 0 0 0 0 0 0 0 0 0 0\n"));
        }
        stat.push_str("intr 0\nctxt 0\n");
        std::fs::write(self.proc_root.join("stat"), stat).unwrap();
        let total_kb = 32 * 1024 * 1024;
        std::fs::write(
            self.proc_root.join("meminfo"),
            format!(
                "MemTotal:       {total_kb} kB\nMemFree:        1 kB\nMemAvailable:   {} kB\n",
                total_kb - used_kb
            ),
        )
        .unwrap();
    }
}

fn at(seconds: i64) -> DateTime<Utc> {
    start() + TimeDelta::seconds(seconds)
}

/// One row per minute since the worker started: the mean of the `cpu.stat`
/// deltas over the minute's wall time, and the peak of
/// `memory.current - file + shmem`. The last, partial minute is written when
/// the run ends. The host minute the worker saw whole is offered once.
#[test]
fn sampler_records_one_row_per_minute_with_mean_cores_and_peak_memory() {
    let mut fixture = Fixture::new();
    let run = "jrun-minutes";
    // Cores and host busy cores for each sample interval, by the minute the
    // interval ends in.
    let cores = [2.0, 0.5, 1.0];
    let host_busy = [1, 3, 2];
    let mut sampler = fixture.sampler(run);
    fixture.run_scope(run, 0.0, 0.0, GB, 0);
    fixture.run_host(0, 0, 1024);
    sampler.sample(at(0));
    for seconds in (5..=125).step_by(5) {
        let minute = (seconds / 60) as usize;
        // Minute 0 peaks at 30 s: 3.5 GB charged, 1.5 GB of it page cache.
        let (current, file) = match seconds {
            30 => (7 * GB / 2, 3 * GB / 2),
            90 => (5 * GB / 2, GB / 2),
            _ => (GB, GB / 4),
        };
        fixture.run_scope(run, 5.0, cores[minute], current, file);
        fixture.run_host(5, host_busy[minute], (seconds as u64) * 1024);
        sampler.sample(at(seconds));
    }
    fixture.run_scope(run, 2.0, cores[2], GB, GB / 4);
    fixture.run_host(2, host_busy[2], 1024);
    sampler.finish(at(127));

    let rows = fixture.store.run_resource_minutes("ws_test", run).unwrap();
    assert_eq!(
        rows.iter().map(|row| row.minute_index).collect::<Vec<_>>(),
        [0, 1, 2],
        "{rows:#?}"
    );
    for (row, (expected_cores, expected_memory)) in
        rows.iter()
            .zip([(2.0, 2 * GB), (0.5, 2 * GB), (1.0, 3 * GB / 4)])
    {
        assert!(
            (row.cpu_cores - expected_cores).abs() < 1e-6,
            "minute {} cores {} != {expected_cores}",
            row.minute_index,
            row.cpu_cores
        );
        assert_eq!(
            row.memory_bytes, expected_memory,
            "minute {}",
            row.minute_index
        );
        assert_eq!(
            row.minute_started_at,
            start() + TimeDelta::minutes(i64::from(row.minute_index))
        );
        assert_eq!(row.task_id, "ORB-1");
        assert_eq!(row.work_class, "change");
        assert_eq!(row.complexity.as_deref(), Some("hard"));
        assert!(!row.resumed);
        assert_eq!(
            row.delivery_admitted_at,
            Some(start() - TimeDelta::minutes(3))
        );
    }
    assert_eq!(
        rows.iter().map(|row| row.samples).collect::<Vec<_>>(),
        [12, 12, 3]
    );

    // 08:00 began before the first reading and 08:02 never ended for this
    // worker; only 08:01 was seen whole.
    let host = fixture.store.host_resource_minutes(start()).unwrap();
    assert_eq!(host.len(), 1, "{host:#?}");
    assert_eq!(host[0].minute_at, at(60));
    assert!((host[0].cpu_cores - 3.0).abs() < 1e-6, "{host:#?}");
    assert_eq!(host[0].cpu_count, 4);
    assert_eq!(host[0].memory_bytes, 115 * 1024 * 1024);
    assert_eq!(host[0].memory_total_bytes, 32 * 1024 * 1024 * 1024);
    assert_eq!(host[0].recorded_by_run_id.as_deref(), Some(run));

    // A second worker for the same run (the first died) continues after the
    // recorded minutes and marks the run resumed, never overwriting them.
    let mut restarted = fixture.sampler(run);
    restarted.finish(at(10));
    let rows = fixture.store.run_resource_minutes("ws_test", run).unwrap();
    assert_eq!(rows.len(), 4);
    assert!(!rows[0].resumed && rows[3].resumed && rows[3].minute_index == 3);
}

/// Page cache charged to a scope is reclaimable and not counted; tmpfs and
/// shared memory inside `file` cannot be dropped and are.
#[test]
fn memory_counts_shmem_but_not_page_cache() {
    let fixture = Fixture::new();
    let page_cache = "jrun-cache";
    let _ = fixture.scope(page_cache);
    // dk-server-1, 2026-10-12: 2.8 GB current, 1.9 GB of it file.
    fixture.write_scope(page_cache, 0, 2_800_000_000, 1_900_000_000, 0);
    fixture.sampler(page_cache).finish(at(1));
    let tmpfs = "jrun-tmpfs";
    let _ = fixture.scope(tmpfs);
    // 1.3 GB of file, 1 GB of it tmpfs written by box tests.
    fixture.write_scope(tmpfs, 0, 1_500_000_000, 1_300_000_000, GB);
    fixture.sampler(tmpfs).finish(at(1));

    let recorded = |run: &str| {
        let rows = fixture.store.run_resource_minutes("ws_test", run).unwrap();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        rows[0].memory_bytes
    };
    assert_eq!(recorded(page_cache), 900_000_000);
    assert_eq!(recorded(tmpfs), 1_200_000_000);
}

/// Every worker offers the host minute; concurrent offers through separate
/// connections store exactly one row for it.
#[test]
fn concurrent_workers_store_one_host_row_per_minute() {
    let mut fixture = Fixture::new();
    let runs = ["jrun-a", "jrun-b"];
    let mut samplers: Vec<_> = runs
        .iter()
        .map(|run| {
            let sampler = fixture.sampler(run);
            fixture.write_scope(run, 0, GB, 0, 0);
            sampler
        })
        .collect();
    for seconds in (55..=185).step_by(5) {
        fixture.run_host(5, 2, 1024);
        for sampler in &mut samplers {
            sampler.sample(at(seconds));
        }
    }
    let minutes = fixture.store.host_resource_minutes(start()).unwrap();
    assert_eq!(
        minutes
            .iter()
            .map(|minute| minute.minute_at)
            .collect::<Vec<_>>(),
        [at(60), at(120)],
        "{minutes:#?}"
    );
    assert_eq!(minutes[0].recorded_by_run_id.as_deref(), Some("jrun-a"));

    let barrier = Arc::new(Barrier::new(6));
    let offers: Vec<_> = (0..6)
        .map(|worker| {
            let barrier = Arc::clone(&barrier);
            let database = fixture.database.clone();
            std::thread::spawn(move || offer(&database, worker, &barrier))
        })
        .collect();
    let stored = offers
        .into_iter()
        .map(|offer| offer.join().unwrap())
        .filter(|stored| *stored)
        .count();
    assert_eq!(stored, 1, "the first insert for a minute wins");
    assert_eq!(
        fixture.store.host_resource_minutes(at(600)).unwrap().len(),
        1
    );
}

fn offer(database: &Path, worker: u32, barrier: &Barrier) -> bool {
    let store = Store::open(database).unwrap();
    barrier.wait();
    store
        .offer_host_resource_minute(&HostResourceMinute {
            minute_at: at(600),
            cpu_cores: f64::from(worker),
            cpu_count: 4,
            memory_bytes: GB,
            memory_total_bytes: 32 * GB,
            samples: 12,
            recorded_by_run_id: Some(format!("jrun-{worker}")),
        })
        .unwrap()
}
