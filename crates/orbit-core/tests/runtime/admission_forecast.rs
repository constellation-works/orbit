//! The host load forecast and the host admission lock through the public
//! runtime: running deliveries scaled by their profile's survival with a flat
//! tail past it, reservations from minute zero, a baseline from the last
//! settled host minute outside tasks, else measured from `/proc/stat` and
//! `/proc/meminfo` rather than `/proc/loadavg`, every
//! workspace on the store, reservation expiry, fresh reads on every call, and
//! two admitter processes that cannot both fill one gap.
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, DurationRound, Utc};
use orbit_core::OrbitRuntime;
use orbit_core::runtime::host_resource::forecast::{
    AdmissionReservationRequest, ForecastDecision, HostLoadForecast,
};
use orbit_core::runtime::host_resource::profile::{ProfileLookup, ProfileSource};
use orbit_store::Store;
use orbit_store::contracts::{
    AdmissionLoadQuery, AdmissionLoadStoreBackend, HostResourceMinute, NewAdmissionReservation,
    ResourceUsageStoreBackend, RunResourceMinute,
};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};
use tempfile::TempDir;

const GIB: u64 = 1024 * 1024 * 1024;
const GIB_F: f64 = GIB as f64;
const OTHER_WORKSPACE: &str = "ws_other_checkout";

struct Fixture {
    root: TempDir,
    global: PathBuf,
    workspace_root: PathBuf,
    proc_root: PathBuf,
    runtime: OrbitRuntime,
    store: Store,
    workspace: String,
    next: Cell<i64>,
}

impl Fixture {
    /// Profiles from one delivery, a ten-minute horizon, and a host whose
    /// `/proc` holds nothing until a test writes it.
    fn new() -> Self {
        let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let workspace_root = root.path().join("repo/.orbit");
        let proc_root = root.path().join("proc");
        for dir in [&global, &workspace_root, &proc_root] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(
            global.join("config.toml"),
            "[machine.auto_concurrency]\nmin_profile_deliveries = 1\nhorizon_cap_minutes = 10\n",
        )
        .unwrap();
        let runtime = OrbitRuntime::from_roots(&global, &workspace_root)
            .unwrap()
            .with_host_load_proc_root(proc_root.clone());
        let store = runtime.sqlite_store().unwrap();
        let workspace = runtime.workspace_id().unwrap();
        Self {
            root,
            global,
            workspace_root,
            proc_root,
            runtime,
            store,
            workspace,
            next: Cell::new(0),
        }
    }

    fn run_id(&self) -> String {
        let index = self.next.get();
        self.next.set(index + 1);
        format!("jrun-forecast-{index}")
    }

    fn job_run(
        &self,
        workspace: &str,
        task: &str,
        created_at: DateTime<Utc>,
        state: JobRunState,
        finished_at: Option<DateTime<Utc>>,
    ) -> String {
        let run = JobRun {
            executed_on: None,
            run_id: self.run_id(),
            job_id: "task_pr_pipeline".into(),
            attempt: 1,
            state,
            scheduled_at: created_at,
            started_at: Some(created_at),
            finished_at,
            duration_ms: None,
            created_at,
            pid: None,
            pid_start_time: None,
            input: Some(json!({"task_id": task})),
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        };
        self.store
            .upsert_job_run_for_workspace(workspace, &run, None)
            .unwrap();
        run.run_id
    }

    #[allow(clippy::too_many_arguments)]
    fn minutes(
        &self,
        workspace: &str,
        run_id: &str,
        task: &str,
        complexity: Option<&str>,
        admitted: DateTime<Utc>,
        cpu: &[f64],
        memory: &[u64],
    ) {
        for (index, (cores, bytes)) in cpu.iter().zip(memory).enumerate() {
            self.store
                .record_run_resource_minute(&RunResourceMinute {
                    workspace_id: workspace.to_string(),
                    run_id: run_id.to_string(),
                    minute_index: index as u32,
                    minute_started_at: admitted + Duration::minutes(index as i64),
                    task_id: task.to_string(),
                    work_class: "change".to_string(),
                    complexity: complexity.map(str::to_string),
                    resumed: false,
                    delivery_admitted_at: Some(admitted),
                    cpu_cores: *cores,
                    cpu_accounting: Some(orbit_store::contracts::CPU_ACCOUNTING_LINUX.into()),
                    memory_bytes: *bytes,
                    samples: 12,
                })
                .unwrap();
        }
    }

    /// One finished `change` delivery, admitted days ago, lasting one minute
    /// per sample.
    fn finished_delivery(&self, task: &str, complexity: &str, cpu: &[f64], memory: &[u64]) {
        let admitted = Utc::now() - Duration::days(2) + Duration::hours(self.next.get());
        let finished = admitted + Duration::minutes(cpu.len() as i64);
        let run = self.job_run(
            &self.workspace,
            task,
            admitted,
            JobRunState::Success,
            Some(finished),
        );
        let workspace = self.workspace.clone();
        self.minutes(
            &workspace,
            &run,
            task,
            Some(complexity),
            admitted,
            cpu,
            memory,
        );
    }

    /// A live delivery admitted `age` before `now`, whose newest minute
    /// measured `cores` and `bytes`.
    #[allow(clippy::too_many_arguments)]
    fn running_delivery(
        &self,
        workspace: &str,
        task: &str,
        complexity: Option<&str>,
        now: DateTime<Utc>,
        age: Duration,
        cores: f64,
        bytes: u64,
    ) {
        let admitted = now - age;
        let run = self.job_run(workspace, task, admitted, JobRunState::Running, None);
        self.minutes(
            workspace,
            &run,
            task,
            complexity,
            admitted,
            &[cores],
            &[bytes],
        );
    }

    /// The host's recorded minute at `at`, of 15 CPUs and 32 GiB.
    fn host_minute(&self, at: DateTime<Utc>, cores: f64, bytes: u64) {
        assert!(
            self.store
                .offer_host_resource_minute(&HostResourceMinute {
                    minute_at: at,
                    cpu_cores: cores,
                    cpu_count: 15,
                    memory_bytes: bytes,
                    memory_total_bytes: 32 * GIB,
                    samples: 12,
                    recorded_by_run_id: None,
                })
                .unwrap()
        );
    }

    fn forecast_at(&self, now: DateTime<Utc>) -> HostLoadForecast {
        self.runtime
            .admit_against_forecast_at(now, |forecast| {
                Ok(ForecastDecision {
                    outcome: forecast.clone(),
                    admitted: Vec::new(),
                })
            })
            .unwrap()
    }

    fn reserve_at(&self, now: DateTime<Utc>, task: &str, complexity: &str) {
        self.runtime
            .admit_against_forecast_at(now, |_| {
                Ok(ForecastDecision {
                    outcome: (),
                    admitted: vec![self.request(task, complexity)],
                })
            })
            .unwrap();
    }

    fn request(&self, task: &str, complexity: &str) -> AdmissionReservationRequest {
        AdmissionReservationRequest {
            task_id: task.to_string(),
            lookup: ProfileLookup::new(
                self.workspace.clone(),
                "change",
                Some(complexity.to_string()),
                false,
            ),
        }
    }

    /// `/proc/stat` with `busy` and `idle` ticks over 15 CPUs, `/proc/meminfo`
    /// with `used` of 32 GiB in use, and a saturated `/proc/loadavg`.
    fn write_proc(&self, busy: u64, idle: u64, used: u64) {
        let mut stat = format!("cpu  {busy} 0 0 {idle} 0 0 0 0 0 0\n");
        for cpu in 0..15 {
            stat.push_str(&format!("cpu{cpu} 0 0 0 0 0 0 0 0 0 0\n"));
        }
        std::fs::write(self.proc_root.join("stat"), stat).unwrap();
        let total_kb = 32 * GIB / 1024;
        let available_kb = (32 * GIB - used) / 1024;
        std::fs::write(
            self.proc_root.join("meminfo"),
            format!("MemTotal: {total_kb} kB\nMemFree: 1024 kB\nMemAvailable: {available_kb} kB\n"),
        )
        .unwrap();
        std::fs::write(
            self.proc_root.join("loadavg"),
            "36.00 35.50 34.00 40/1200 99999\n",
        )
        .unwrap();
    }
}

fn assert_series(actual: &[f64], expected: &[f64], what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: {actual:?}");
    for (minute, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{what} at +{minute}m: {actual} != {expected}"
        );
    }
}

fn reserved(forecast: &HostLoadForecast) -> Vec<&str> {
    forecast
        .reservations
        .iter()
        .map(|reservation| reservation.task_id.as_str())
        .collect()
}

/// `hard` deliveries last 1, 2 and 4 minutes, so `alive` is 1, 2/3, 1/3, 1/3.
/// One running for a minute is expected at `cpu(1+t) · alive(1+t) / (2/3)`;
/// a `small` one past its two-minute profile holds its measured use; a
/// reserved `small` task adds its profile from minute zero.
fn the_forecast_scales_running_deliveries_by_survival_and_adds_reservations() {
    let fixture = Fixture::new();
    fixture.finished_delivery("ORB-h1", "hard", &[6.0], &[GIB]);
    fixture.finished_delivery("ORB-h2", "hard", &[3.0, 3.0], &[2 * GIB, 2 * GIB]);
    fixture.finished_delivery("ORB-h3", "hard", &[1.0; 4], &[4 * GIB; 4]);
    fixture.finished_delivery("ORB-s1", "small", &[0.5, 3.0], &[GIB, 2 * GIB]);
    let now = Utc::now();
    let workspace = fixture.workspace.clone();
    fixture.running_delivery(
        &workspace,
        "ORB-x",
        Some("hard"),
        now,
        Duration::seconds(70),
        1.5,
        3 * GIB,
    );
    fixture.running_delivery(
        &workspace,
        "ORB-y",
        Some("small"),
        now,
        Duration::seconds(310),
        0.75,
        GIB / 2,
    );

    let before = fixture.forecast_at(now);
    assert_eq!(before.horizon_minutes, 10);
    assert!(
        before.host.is_none() && before.cpu.baseline.is_none(),
        "an unreadable host has no baseline"
    );
    let x = before
        .running
        .iter()
        .find(|d| d.task_id == "ORB-x")
        .unwrap();
    assert_eq!(x.age_minutes, 1);
    assert_eq!(x.profile_source, ProfileSource::Exact);
    assert!(!x.held_flat);
    let y = before
        .running
        .iter()
        .find(|d| d.task_id == "ORB-y")
        .unwrap();
    assert_eq!(y.age_minutes, 5);
    assert!(y.held_flat, "five minutes is past the small profile's last");
    // x: cpu(1) = mean(3, 1) = 2, then cpu 1 at alive (1/3)/(2/3); y: 0.75.
    let tail = [0.75; 8];
    assert_series(
        &before.cpu.minutes,
        &[&[2.75, 1.25, 1.25][..], &tail].concat(),
        "cpu before the reservation",
    );

    fixture.reserve_at(now, "ORB-r", "small");
    let forecast = fixture.forecast_at(now);
    assert_eq!(reserved(&forecast), ["ORB-r"]);
    assert_eq!(
        forecast.reservations[0].profile_source,
        ProfileSource::Exact
    );
    // The reservation adds the small profile, 0.5 then 3 cores, from minute 0.
    assert_series(
        &forecast.cpu.minutes,
        &[&[3.25, 4.25, 1.25][..], &tail].concat(),
        "cpu",
    );
    assert_eq!(forecast.cpu.peak.minute, 1);
    assert!((forecast.cpu.peak.value - 4.25).abs() < 1e-6);
    // x: p90 memory 4 GiB at minutes 1 to 3, scaled by 1, 1/2, 1/2; y holds
    // 0.5 GiB; the reservation adds 1 then 2 GiB.
    let memory: Vec<f64> = forecast.memory.minutes.iter().map(|b| b / GIB_F).collect();
    assert_series(
        &memory,
        &[&[5.5, 4.5, 2.5][..], &[0.5; 8]].concat(),
        "memory GiB",
    );
    assert_eq!(forecast.memory.peak.minute, 0);
}

/// A live leaf of another workspace on the same `orbit.db` is part of the
/// host's load.
fn a_live_leaf_of_another_workspace_counts() {
    let fixture = Fixture::new();
    assert_ne!(fixture.workspace, OTHER_WORKSPACE);
    let now = Utc::now();
    fixture.running_delivery(
        OTHER_WORKSPACE,
        "ORB-elsewhere",
        Some("medium"),
        now,
        Duration::minutes(3),
        2.0,
        GIB,
    );

    let forecast = fixture.forecast_at(now);
    let elsewhere = forecast
        .running
        .iter()
        .find(|delivery| delivery.workspace_id == OTHER_WORKSPACE)
        .expect("the other workspace's delivery is running");
    assert_eq!(elsewhere.task_id, "ORB-elsewhere");
    assert_eq!(
        elsewhere.profile_source,
        ProfileSource::None,
        "this host has no profiles, so it holds its measured use"
    );
    assert_series(&forecast.cpu.minutes, &[2.0; 11], "cpu");
}

/// The baseline is `/proc/stat` utilization and `MemTotal - MemAvailable`
/// minus the running deliveries' newest minutes, held flat. A load average of
/// 36 on 15 cores with 6 busy cores leaves 2 cores, not 32.
fn the_baseline_is_measured_utilization_minus_running_use() {
    let fixture = Fixture::new();
    let now = Utc::now();
    let workspace = fixture.workspace.clone();
    fixture.running_delivery(
        &workspace,
        "ORB-busy",
        None,
        now,
        Duration::minutes(2),
        4.0,
        8 * GIB,
    );
    fixture.write_proc(10_000, 90_000, 12 * GIB);

    let first = fixture.forecast_at(now);
    assert_eq!(
        first.cpu.baseline, None,
        "no ticks passed since the only reading: CPU is unknown, not zero"
    );
    assert_eq!(first.memory.baseline, Some(4.0 * GIB_F));

    // 600 of 1,500 ticks busy over 15 CPUs: 6 cores.
    fixture.write_proc(10_600, 90_900, 12 * GIB);
    let forecast = fixture.forecast_at(now);
    let host = forecast.host.unwrap();
    assert!((host.cpu_cores.unwrap() - 6.0).abs() < 1e-9);
    let baseline = forecast.cpu.baseline.unwrap();
    assert!(
        (baseline - 2.0).abs() < 1e-9,
        "the baseline is 6 measured cores minus 4 of the delivery, not the load average: {baseline}"
    );
    assert_series(&forecast.cpu.minutes, &[6.0; 11], "cpu");
    assert_eq!(forecast.memory.baseline, Some(4.0 * GIB_F));
    assert_series(
        &forecast
            .memory
            .minutes
            .iter()
            .map(|b| b / GIB_F)
            .collect::<Vec<_>>(),
        &[12.0; 11],
        "memory GiB",
    );
}

/// The baseline is the newest completed host minute whose run minutes are
/// all recorded, minus those run minutes weighted by their overlap with it,
/// so a momentary spike in the reading now adds nothing at any minute.
/// Regression: a 250 ms sample of 14 busy cores became a flat 10-core
/// baseline while the measured minute held 3 cores outside tasks.
fn the_baseline_is_the_last_settled_minute_outside_tasks_not_a_spike() {
    let fixture = Fixture::new();
    fixture.finished_delivery("ORB-p", "medium", &[4.0; 20], &[2 * GIB; 20]);
    let minute = Utc::now().duration_trunc(Duration::minutes(1)).unwrap() - Duration::minutes(3);
    let now = minute + Duration::seconds(200);
    let workspace = fixture.workspace.clone();
    // Running since 30 s before the minute: half of two run minutes fall in it.
    let live = fixture.job_run(
        &workspace,
        "ORB-live",
        minute - Duration::seconds(30),
        JobRunState::Running,
        None,
    );
    fixture.minutes(
        &workspace,
        &live,
        "ORB-live",
        Some("medium"),
        minute - Duration::seconds(30),
        &[4.0; 3],
        &[2 * GIB; 3],
    );
    // A delivery that ran through the minute and has since finished.
    let done = fixture.job_run(
        &workspace,
        "ORB-done",
        minute,
        JobRunState::Success,
        Some(minute + Duration::minutes(1)),
    );
    fixture.minutes(
        &workspace,
        &done,
        "ORB-done",
        Some("small"),
        minute,
        &[2.0],
        &[GIB],
    );
    fixture.host_minute(minute, 9.0, 12 * GIB);
    // Too recent: a run minute overlapping it may not be written yet.
    fixture.host_minute(minute + Duration::minutes(2), 15.0, 30 * GIB);

    let unreadable = fixture.forecast_at(now);
    assert!(
        unreadable.cpu.baseline.is_none() && unreadable.baseline_minute.is_none(),
        "an unreadable host still has no baseline"
    );

    fixture.write_proc(10_000, 90_000, 30 * GIB);
    fixture.forecast_at(now);
    // 1,400 of 1,500 ticks busy over 15 CPUs: a 14-core spike.
    fixture.write_proc(11_400, 90_100, 30 * GIB);
    let forecast = fixture.forecast_at(now);
    assert!((forecast.host.unwrap().cpu_cores.unwrap() - 14.0).abs() < 1e-9);
    assert_eq!(forecast.baseline_minute, Some(minute));
    let baseline = forecast.cpu.baseline.unwrap();
    assert!(
        (baseline - 3.0).abs() < 1e-9,
        "9 host cores minus 2 + 2 of the live delivery and 2 of the finished one: {baseline}"
    );
    // The live delivery at age 3 adds its profile's 4 cores and 2 GiB.
    assert_series(&forecast.cpu.minutes, &[7.0; 11], "cpu");
    assert_eq!(forecast.memory.baseline, Some(9.0 * GIB_F));
    assert_series(
        &forecast
            .memory
            .minutes
            .iter()
            .map(|b| b / GIB_F)
            .collect::<Vec<_>>(),
        &[11.0; 11],
        "memory GiB",
    );

    let later = fixture.forecast_at(now + Duration::minutes(10));
    assert_eq!(
        later.baseline_minute, None,
        "a host minute older than five minutes leaves the baseline to the reading now"
    );
}

/// With no settled host minute, a delivery that has recorded no minute yet
/// is counted in the reading now at its profile's expectation for its age,
/// so its startup load is not also held flat in the baseline.
/// Regression: a just-created delivery subtracted nothing while its full
/// profile was added.
fn a_delivery_with_no_minute_yet_counts_its_profile_in_the_reading_now() {
    let fixture = Fixture::new();
    fixture.finished_delivery(
        "ORB-p",
        "medium",
        &[3.0, 5.0, 5.0, 1.0],
        &[GIB, 3 * GIB, 3 * GIB, GIB],
    );
    let now = Utc::now();
    let workspace = fixture.workspace.clone();
    fixture.job_run(
        &workspace,
        "ORB-young",
        now - Duration::seconds(10),
        JobRunState::Running,
        None,
    );
    fixture.write_proc(10_000, 90_000, 5 * GIB);
    fixture.forecast_at(now);
    // 500 of 1,500 ticks busy over 15 CPUs: 2 cores outside tasks plus the
    // delivery's first-minute 3.
    fixture.write_proc(10_500, 91_000, 5 * GIB);
    let forecast = fixture.forecast_at(now);
    assert_eq!(forecast.baseline_minute, None);
    let young = &forecast.running[0];
    assert_eq!((young.age_minutes, young.held_flat), (0, false));
    let baseline = forecast.cpu.baseline.unwrap();
    assert!(
        (baseline - 2.0).abs() < 1e-9,
        "5 measured cores minus the profile's 3 at age 0: {baseline}"
    );
    assert_series(
        &forecast.cpu.minutes,
        &[&[5.0, 7.0, 7.0, 3.0][..], &[2.0; 7]].concat(),
        "cpu",
    );
    assert_eq!(forecast.memory.baseline, Some(4.0 * GIB_F));
}

/// A reservation stops counting once a delivery run naming its task is
/// created in its workspace after it, or five minutes after it was written.
fn a_reservation_ends_when_its_run_exists_or_after_five_minutes() {
    let fixture = Fixture::new();
    fixture.finished_delivery("ORB-p", "small", &[1.0, 1.0], &[GIB, GIB]);
    let now = Utc::now();
    let workspace = fixture.workspace.clone();
    fixture.reserve_at(now, "ORB-dispatched", "small");
    fixture.reserve_at(now, "ORB-failed", "small");
    // Neither an earlier delivery of the task nor a run in another workspace
    // is the reserved run.
    fixture.job_run(
        &workspace,
        "ORB-dispatched",
        now - Duration::hours(1),
        JobRunState::Success,
        Some(now - Duration::minutes(30)),
    );
    fixture.job_run(
        OTHER_WORKSPACE,
        "ORB-dispatched",
        now + Duration::seconds(20),
        JobRunState::Failed,
        Some(now + Duration::seconds(25)),
    );
    let forecast = fixture.forecast_at(now + Duration::minutes(1));
    assert_eq!(reserved(&forecast), ["ORB-dispatched", "ORB-failed"]);
    assert_series(
        &forecast.cpu.minutes,
        &[&[2.0, 2.0][..], &[0.0; 9]].concat(),
        "cpu",
    );

    // The run exists, even though it already ended.
    fixture.job_run(
        &workspace,
        "ORB-dispatched",
        now + Duration::seconds(30),
        JobRunState::Failed,
        Some(now + Duration::seconds(40)),
    );
    let forecast = fixture.forecast_at(now + Duration::minutes(1));
    assert_eq!(reserved(&forecast), ["ORB-failed"]);
    assert!(forecast.running.is_empty());

    let forecast = fixture.forecast_at(now + Duration::minutes(5) - Duration::seconds(1));
    assert_eq!(reserved(&forecast), ["ORB-failed"]);
    let forecast = fixture.forecast_at(now + Duration::minutes(5) + Duration::seconds(1));
    assert!(
        reserved(&forecast).is_empty(),
        "dispatch failed five minutes ago"
    );
    assert_series(&forecast.cpu.minutes, &[0.0; 11], "cpu");
}

/// Nothing a forecast reads about the host's live load is kept between
/// calls: a delivery started, a reservation another connection wrote and the
/// host's memory all show in the next call, though no delivery finished to
/// change the profiles.
fn every_call_reads_running_reservations_and_baseline_fresh() {
    let fixture = Fixture::new();
    fixture.finished_delivery("ORB-p", "small", &[1.0, 1.0], &[GIB, GIB]);
    fixture.write_proc(10_000, 90_000, 4 * GIB);
    let now = Utc::now();
    let first = fixture.forecast_at(now);
    assert!(first.running.is_empty() && first.reservations.is_empty());
    assert_eq!(first.memory.baseline, Some(4.0 * GIB_F));

    let workspace = fixture.workspace.clone();
    fixture.running_delivery(
        &workspace,
        "ORB-started",
        Some("small"),
        now,
        Duration::seconds(10),
        0.5,
        GIB,
    );
    // Another drain's own connection to the same database.
    let other = Store::open(&fixture.runtime.global_root().join("orbit.db")).unwrap();
    other
        .admit_under_host_lock(
            &AdmissionLoadQuery {
                now,
                delivery_jobs: &["task_pr_pipeline"],
                reservations_expire_before: now - Duration::minutes(5),
            },
            &mut |_| {
                Ok(vec![NewAdmissionReservation {
                    workspace_id: OTHER_WORKSPACE.to_string(),
                    task_id: "ORB-elsewhere".to_string(),
                    work_class: "change".to_string(),
                    complexity: Some("small".to_string()),
                    base_class: "change".to_string(),
                }])
            },
        )
        .unwrap();
    fixture.write_proc(10_000, 90_000, 10 * GIB);

    let second = fixture.forecast_at(now);
    assert_eq!(second.running.len(), 1);
    assert_eq!(second.running[0].task_id, "ORB-started");
    assert_eq!(reserved(&second), ["ORB-elsewhere"]);
    assert_eq!(second.memory.baseline, Some(9.0 * GIB_F));
}

/// Set in the second admitter's process: the rendezvous directory.
const RACER_ENV: &str = "ORBIT_TEST_ADMISSION_RACER";
/// The isolation marker `dispatch_admission::isolated` honours.
const ISOLATION_MARKER: &str = "ORBIT_TEST_DISPATCH_ADMISSION_CHILD";
/// Cores free on the host: room for one four-core delivery, not two.
const GAP_CORES: f64 = 6.0;

/// Admit `task` when its profile fits under [`GAP_CORES`] at every minute.
fn admit_if_it_fits(
    forecast: &HostLoadForecast,
    workspace: &str,
    task: &str,
) -> ForecastDecision<bool> {
    let lookup = ProfileLookup::new(workspace, "change", Some("medium".to_string()), false);
    let candidate = forecast.profiles.lookup(&lookup).profile.unwrap();
    let fits = forecast.cpu.minutes.iter().enumerate().all(|(t, load)| {
        let own = candidate
            .series
            .get(t)
            .map_or(0.0, |minute| minute.cpu_cores * minute.alive);
        load + own <= GAP_CORES
    });
    ForecastDecision {
        outcome: fits,
        admitted: if fits {
            vec![AdmissionReservationRequest {
                task_id: task.to_string(),
                lookup,
            }]
        } else {
            Vec::new()
        },
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + StdDuration::from_secs(60);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(StdDuration::from_millis(10));
    }
}

/// Two admitter processes race for a gap that fits one four-core delivery.
/// The first holds the host lock while the second asks; the second waits for
/// it, sees its reservation and admits nothing.
#[test]
fn two_admitter_processes_cannot_fill_one_gap_twice() {
    const TEST: &str = "admission_forecast::two_admitter_processes_cannot_fill_one_gap_twice";
    if !super::dispatch_admission::isolated(TEST) {
        return;
    }
    if let Ok(rendezvous) = std::env::var(RACER_ENV) {
        second_admitter(Path::new(&rendezvous));
        return;
    }
    let fixture = Fixture::new();
    fixture.finished_delivery("ORB-p", "medium", &[4.0; 3], &[GIB; 3]);
    let rendezvous = fixture.root.path().join("race");
    std::fs::create_dir_all(&rendezvous).unwrap();
    std::fs::write(
        rendezvous.join("roots.json"),
        json!({
            "global": fixture.global,
            "workspace": fixture.workspace_root,
            "proc": fixture.proc_root,
        })
        .to_string(),
    )
    .unwrap();
    let racer = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
        .env(ISOLATION_MARKER, TEST)
        .env(RACER_ENV, &rendezvous)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    wait_for(&rendezvous.join("ready"));

    let first = fixture
        .runtime
        .admit_against_forecast(|forecast| {
            std::fs::write(rendezvous.join("holding"), "").unwrap();
            // The second admitter asks for the lock while this one holds it.
            std::thread::sleep(StdDuration::from_secs(1));
            Ok(admit_if_it_fits(forecast, &fixture.workspace, "ORB-first"))
        })
        .unwrap();

    let output = racer.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "second admitter failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let second: Value =
        serde_json::from_slice(&std::fs::read(rendezvous.join("result.json")).unwrap()).unwrap();
    assert!(first, "the empty host fits the first delivery");
    assert!(
        second["waited_ms"].as_u64().unwrap() >= 500,
        "the second admitter asked while the first held the lock: {second}"
    );
    assert_eq!(second["seen"], json!(["ORB-first"]), "{second}");
    assert_eq!(second["admitted"], json!(false), "{second}");
}

fn second_admitter(rendezvous: &Path) {
    let roots: Value =
        serde_json::from_slice(&std::fs::read(rendezvous.join("roots.json")).unwrap()).unwrap();
    let path = |key: &str| PathBuf::from(roots[key].as_str().unwrap());
    let runtime = OrbitRuntime::from_roots(&path("global"), &path("workspace"))
        .unwrap()
        .with_host_load_proc_root(path("proc"));
    let workspace = runtime.workspace_id().unwrap();
    std::fs::write(rendezvous.join("ready"), "").unwrap();
    wait_for(&rendezvous.join("holding"));
    let asked = Instant::now();
    let (admitted, seen) = runtime
        .admit_against_forecast(|forecast| {
            let seen: Vec<String> = forecast
                .reservations
                .iter()
                .map(|reservation| reservation.task_id.clone())
                .collect();
            let decision = admit_if_it_fits(forecast, &workspace, "ORB-second");
            Ok(ForecastDecision {
                outcome: (decision.outcome, seen),
                admitted: decision.admitted,
            })
        })
        .unwrap();
    std::fs::write(
        rendezvous.join("result.json"),
        json!({
            "admitted": admitted,
            "seen": seen,
            "waited_ms": u64::try_from(asked.elapsed().as_millis()).unwrap(),
        })
        .to_string(),
    )
    .unwrap();
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    the_forecast_scales_running_deliveries_by_survival_and_adds_reservations_scenarios,
    the_forecast_scales_running_deliveries_by_survival_and_adds_reservations,
    a_live_leaf_of_another_workspace_counts,
    the_baseline_is_measured_utilization_minus_running_use,
    the_baseline_is_the_last_settled_minute_outside_tasks_not_a_spike,
    a_delivery_with_no_minute_yet_counts_its_profile_in_the_reading_now,
    a_reservation_ends_when_its_run_exists_or_after_five_minutes,
    every_call_reads_running_reservations_and_baseline_fresh
);
