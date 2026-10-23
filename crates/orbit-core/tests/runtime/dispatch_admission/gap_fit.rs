//! Gap-fit admission in a local drain (`machine.auto_concurrency.mode`).
//!
//! Every fixture is a host of 10 cores and 32 GiB with 2 GiB in use, gap fit
//! admitting up to 80% CPU (8 cores) and 75% memory, and one-delivery `change`
//! profiles: `hard` is 1, 1, 1, 6, 1 cores, `medium` 1, 6, 1 and `low` 0.5,
//! 0.5, each at 1 GiB a minute. A `hard` delivery running for two minutes
//! puts its 6-core spike at +1m, where `medium` would put its own.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::host_resource::forecast::{ForecastDecision, HostLoadForecast};
use orbit_core::{OrbitRuntime, TaskComplexity, TaskPriority, TaskStatus, TaskType};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{
    AdmissionForecastPoint, AdmissionRunContext, ExecutionClaim, ExecutionClaimPhase,
    ExecutionLocation, FrictionListFilter, ResourceUsageStoreBackend, RunResourceMinute,
};
use orbit_store::friction_store::FrictionStore;
use orbit_tools::ToolContext;
use orbit_types::record::FrictionRecord;
use orbit_types::workflow::{
    DrainAdmissionForecast, DrainPassCounters, DrainPassOutcome, JobRun, JobRunState,
};
use serde_json::{Value, json};
use tempfile::TempDir;

use super::{PressureProbe, isolated, running_run};

const GIB: u64 = 1024 * 1024 * 1024;

struct Fixture {
    _root: TempDir,
    global: PathBuf,
    workspace: PathBuf,
    proc_root: PathBuf,
    probe: std::sync::Arc<PressureProbe>,
    runtime: OrbitRuntime,
    next: std::cell::Cell<u32>,
}

impl Fixture {
    /// A host in `mode`, with `extra` config lines, and no profiles yet.
    fn new(mode: &str, extra: &str) -> Self {
        let root = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
        let global = root.path().join("home/.orbit");
        let workspace = root.path().join("repo/.orbit");
        let proc_root = root.path().join("proc");
        for dir in [&global, &workspace, &proc_root] {
            std::fs::create_dir_all(dir).unwrap();
        }
        write_proc(&proc_root, 2);
        let probe = PressureProbe::calm();
        let runtime = build(&global, &workspace, &proc_root, &probe, mode, extra);
        Self {
            _root: root,
            global,
            workspace,
            proc_root,
            probe,
            runtime,
            next: std::cell::Cell::new(0),
        }
    }

    /// The same host and store under another mode.
    fn with_mode(&self, mode: &str, extra: &str) -> OrbitRuntime {
        build(
            &self.global,
            &self.workspace,
            &self.proc_root,
            &self.probe,
            mode,
            extra,
        )
    }

    fn workspace_id(&self) -> String {
        self.runtime.workspace_id().unwrap()
    }

    /// The `hard`, `medium` and `low` profiles.
    fn seed_profiles(&self) {
        self.finished_delivery("hard", &[1.0, 1.0, 1.0, 6.0, 1.0]);
        self.finished_delivery("medium", &[1.0, 6.0, 1.0]);
        self.finished_delivery("low", &[0.5, 0.5]);
    }

    /// One finished `change` delivery of `complexity`, admitted days ago and
    /// lasting a minute per sample, at 1 GiB a minute.
    fn finished_delivery(&self, complexity: &str, cpu: &[f64]) {
        self.finished_delivery_using(complexity, cpu, GIB);
    }

    /// [`Self::finished_delivery`] using `memory` bytes a minute.
    fn finished_delivery_using(&self, complexity: &str, cpu: &[f64], memory: u64) {
        let index = self.next.get();
        self.next.set(index + 1);
        let task = format!("ORB-done-{index}");
        let admitted = Utc::now() - Duration::days(2) + Duration::hours(i64::from(index));
        let finished = admitted + Duration::minutes(cpu.len() as i64);
        let run = JobRun {
            executed_on: None,
            run_id: format!("jrun-gap-fit-{index}"),
            job_id: "task_pr_pipeline".into(),
            attempt: 1,
            state: JobRunState::Success,
            scheduled_at: admitted,
            started_at: Some(admitted),
            finished_at: Some(finished),
            duration_ms: None,
            created_at: admitted,
            pid: None,
            pid_start_time: None,
            input: Some(json!({"task_id": task})),
            retry_source_run_id: None,
            knowledge_metrics: None,
            resolved_crew: None,
            crew_model: None,
            steps: Vec::new(),
        };
        let store = self.runtime.sqlite_store().unwrap();
        let workspace = self.workspace_id();
        store
            .upsert_job_run_for_workspace(&workspace, &run, None)
            .unwrap();
        for (minute, cores) in cpu.iter().enumerate() {
            store
                .record_run_resource_minute(&RunResourceMinute {
                    workspace_id: workspace.clone(),
                    run_id: run.run_id.clone(),
                    minute_index: minute as u32,
                    minute_started_at: admitted + Duration::minutes(minute as i64),
                    task_id: task.clone(),
                    work_class: "change".to_string(),
                    complexity: Some(complexity.to_string()),
                    resumed: false,
                    delivery_admitted_at: Some(admitted),
                    cpu_cores: *cores,
                    cpu_accounting: Some(orbit_store::contracts::CPU_ACCOUNTING_LINUX.into()),
                    memory_bytes: memory,
                    samples: 12,
                })
                .unwrap();
        }
    }

    /// A backlog task of `complexity` touching its own file.
    fn task(&self, title: &str, priority: TaskPriority, complexity: TaskComplexity) -> String {
        let file = format!("{title}.txt");
        std::fs::write(self.runtime.paths().repo_root.join(&file), "fixture\n").unwrap();
        self.runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: format!("Fixture task: {title}"),
                acceptance_criteria: vec!["Fixture task is observable.".to_string()],
                plan: "Fixture plan.".to_string(),
                context_files: vec![format!("file:{file}")],
                priority,
                complexity,
                task_type: Some(TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    /// The delivery of `task` ran and ended, so its reservation leaves the
    /// forecast.
    fn delivered(&self, task: &str) {
        let run = running_run(
            &self.runtime,
            "task_auto_pipeline",
            json!({"task_ids": [task]}),
        );
        self.ended(&run);
    }

    /// `run` ends, starting first if it had not.
    fn ended(&self, run: &str) {
        let jobs = orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.workspace_id(),
        );
        if jobs.get_job_run(run).unwrap().unwrap().state == JobRunState::Pending {
            jobs.mark_job_run_running(run, Utc::now(), std::process::id())
                .unwrap();
        }
        jobs.finalize_job_run(run, JobRunState::Success, Utc::now(), Some(0))
            .unwrap();
    }

    /// A `hard` delivery of a new task, admitted two minutes ago, whose live
    /// run has not started: its profile is ahead and it uses nothing yet, so
    /// the host reading is all baseline. Returns its run.
    fn running_hard_delivery(&self) -> String {
        let task = self.task("running", TaskPriority::Low, TaskComplexity::Hard);
        orbit_store::compose::workspace_job_run_store(
            self.runtime.sqlite_store().unwrap(),
            self.workspace_id(),
        )
        .insert_job_run(
            "task_auto_pipeline",
            1,
            Utc::now(),
            Some(json!({
                "task_ids": [task],
                "resource_class": {
                    "task_id": task,
                    "work_class": "change",
                    "complexity": "hard",
                    "admitted_at": Utc::now() - Duration::seconds(130),
                },
            })),
            None,
        )
        .expect("run")
        .run_id
    }

    fn drain(&self) -> String {
        running_run(&self.runtime, "workspace_auto_pipeline", json!({}))
    }

    /// The reservations on the host now.
    fn reserved(&self, runtime: &OrbitRuntime) -> Vec<String> {
        runtime
            .admit_against_forecast(|forecast: &HostLoadForecast| {
                Ok(ForecastDecision {
                    outcome: forecast
                        .reservations
                        .iter()
                        .map(|reservation| reservation.task_id.clone())
                        .collect(),
                    admitted: Vec::new(),
                })
            })
            .unwrap()
    }
}

fn build(
    global: &Path,
    workspace: &Path,
    proc_root: &Path,
    probe: &std::sync::Arc<PressureProbe>,
    mode: &str,
    extra: &str,
) -> OrbitRuntime {
    // A horizon in `extra` replaces the fixtures' 10 minutes.
    let horizon = if extra.contains("horizon_cap_minutes") {
        ""
    } else {
        "horizon_cap_minutes = 10\n"
    };
    std::fs::write(
        global.join("config.toml"),
        format!(
            "[machine.auto_concurrency]\nmode = \"{mode}\"\nmin_profile_deliveries = 1\n\
             {horizon}admit_cpu_percent = 80\nadmit_memory_percent = 75\n{extra}"
        ),
    )
    .unwrap();
    OrbitRuntime::from_roots(global, workspace)
        .unwrap()
        .with_host_resource_probe(probe.clone())
        .with_host_load_proc_root(proc_root.to_path_buf())
}

/// Ten CPUs whose ticks never move, so the CPU baseline is unknown and
/// counts as zero, and `used_gib` of 32 GiB in use.
fn write_proc(proc_root: &Path, used_gib: u64) {
    let mut stat = "cpu  1000 0 0 9000 0 0 0 0 0 0\n".to_string();
    for cpu in 0..10 {
        stat.push_str(&format!("cpu{cpu} 100 0 0 900 0 0 0 0 0 0\n"));
    }
    std::fs::write(proc_root.join("stat"), stat).unwrap();
    let total_kb = 32 * GIB / 1024;
    let available_kb = (32 - used_gib) * GIB / 1024;
    std::fs::write(
        proc_root.join("meminfo"),
        format!("MemTotal: {total_kb} kB\nMemFree: 1024 kB\nMemAvailable: {available_kb} kB\n"),
    )
    .unwrap();
}

fn classify(runtime: &OrbitRuntime, drain: &str, slots: u32) -> Value {
    crate::scenarios::with_immutable_host_load(|| {
        runtime
            .run_deterministic(
                "classify_workspace_auto_tasks",
                &json!({}),
                &json!({
                    "run_id": drain,
                    "max_active_leaf_runs": slots,
                    "poll_sleep_seconds": 7,
                    "idle_sleep_seconds": 70,
                }),
                ToolContext::default(),
            )
            .expect("classify")
    })
}

/// The friction records filed in the fixture's workspace.
fn frictions(runtime: &OrbitRuntime) -> Vec<FrictionRecord> {
    FrictionStore::open(
        runtime.sqlite_store().unwrap(),
        runtime.workspace_id().unwrap(),
        runtime.data_root().join("frictions"),
    )
    .unwrap()
    .list(&FrictionListFilter::default())
    .unwrap()
    .into_iter()
    .map(|stored| stored.record)
    .collect()
}

fn admitted(output: &Value) -> Vec<String> {
    serde_json::from_value(output["loose_task_ids"].clone()).unwrap()
}

/// The pass's recorded block, which must match what the classifier returned.
fn recorded(runtime: &OrbitRuntime, drain: &str, output: &Value) -> DrainAdmissionForecast {
    let record = runtime
        .read_run_state(drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .expect("last pass")
        .admission_forecast
        .expect("the pass records its gap fit");
    assert_eq!(
        serde_json::to_value(&record).unwrap(),
        output["admission_forecast"],
        "{output:#}"
    );
    record
}

/// The drain's admission pass counters.
fn counters(runtime: &OrbitRuntime, drain: &str) -> DrainPassCounters {
    runtime
        .read_run_state(drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_pass_counters
        .expect("the drain counts its passes")
}

fn ids(entries: impl IntoIterator<Item = String>) -> Vec<String> {
    entries.into_iter().collect()
}

/// A `medium` candidate whose spike lands on the running `hard` delivery's
/// is skipped with the minute and resource, a lighter `low` one behind it is
/// admitted in the same pass, and the admission is reserved for the next
/// admitter on the host.
fn enforce_skips_a_colliding_candidate_and_admits_a_lighter_one_behind_it() {
    let fixture = Fixture::new("enforce", "");
    fixture.seed_profiles();
    fixture.running_hard_delivery();
    let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
    let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
    let drain = fixture.drain();

    let pass = classify(&fixture.runtime, &drain, 4);
    assert_eq!(admitted(&pass), vec![light.clone()], "{pass:#}");
    let record = recorded(&fixture.runtime, &drain, &pass);
    assert_eq!(record.mode, "enforce");
    let cpu_peak = record.cpu_peak.as_ref().unwrap();
    assert_eq!(cpu_peak.minute, 1, "{record:#?}");
    assert!((cpu_peak.value - 6.0).abs() < 1e-9, "{record:#?}");
    assert!(
        cpu_peak
            .percent
            .is_some_and(|percent| (percent - 60.0).abs() < 1e-9),
        "{record:#?}"
    );
    let [skip] = record.skipped.as_slice() else {
        panic!("{record:#?}");
    };
    assert_eq!(skip.task_id, heavy);
    assert_eq!(skip.resource.as_deref(), Some("cpu"));
    assert_eq!(skip.minute, Some(1));
    assert_eq!(skip.detail, "cpu 120% at +1m");
    assert_eq!(skip.profile_source, "exact");
    assert_eq!(
        ids(record.admitted.iter().map(|a| a.task_id.clone())),
        vec![light.clone()]
    );
    assert_eq!(record.admitted[0].profile_source, "exact");
    let head = record.head.as_ref().expect("the head failed gap fit");
    assert_eq!(head.task_id, heavy);
    assert!(!head.protected);

    let pass_record = fixture
        .runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .unwrap();
    let waiting = pass_record
        .deferred
        .iter()
        .find(|waiting| waiting.task_id == heavy)
        .expect("the skipped candidate waits");
    assert_eq!(waiting.reason.as_deref(), Some("gap_fit"));
    assert_eq!(waiting.detail.as_deref(), Some("cpu 120% at +1m"));
    assert_eq!(fixture.reserved(&fixture.runtime), vec![light]);
}

/// Each admitted candidate is in the forecast before the next is judged:
/// on an idle host two 3-core `low` deliveries fit under 8 cores, and the
/// third, which would make 9, does not.
fn candidates_admitted_in_one_pass_count_each_other() {
    let fixture = Fixture::new("enforce", "");
    fixture.finished_delivery("low", &[3.0, 3.0]);
    let first = fixture.task("first", TaskPriority::High, TaskComplexity::Low);
    let second = fixture.task("second", TaskPriority::Medium, TaskComplexity::Low);
    let third = fixture.task("third", TaskPriority::Low, TaskComplexity::Low);
    let drain = fixture.drain();

    let pass = classify(&fixture.runtime, &drain, 5);
    assert_eq!(
        admitted(&pass),
        vec![first.clone(), second.clone()],
        "{pass:#}"
    );
    let record = recorded(&fixture.runtime, &drain, &pass);
    assert_eq!(record.skipped.len(), 1, "{record:#?}");
    assert_eq!(record.skipped[0].task_id, third);
    assert_eq!(record.skipped[0].detail, "cpu 90% at +0m");
    let mut reserved = fixture.reserved(&fixture.runtime);
    reserved.sort();
    let mut expected = vec![first, second];
    expected.sort();
    assert_eq!(reserved, expected);
}

/// A candidate gap fit admits and the owner-wave section then refuses, for a
/// live claim on its file, starts no run: the pass leaves no reservation for
/// it, however many passes repeat, so its profile never stacks onto the
/// forecast. A candidate the section admits keeps its one reservation.
fn a_candidate_the_section_refuses_leaves_no_reservation() {
    let fixture = Fixture::new("enforce", "");
    fixture.finished_delivery("low", &[1.0, 1.0]);
    let refused = fixture.task("refused", TaskPriority::High, TaskComplexity::Low);
    let claim = ExecutionClaim {
        claim_id: "fixture-claim".into(),
        task_id: "ORB-claim-holder".into(),
        request_id: "fixture-request".into(),
        executed_on: ExecutionLocation {
            machine_id: "fixture-machine".into(),
            machine_name: None,
        },
        run_context: AdmissionRunContext {
            run_id: "fixture-run".into(),
            job_name: "fixture".into(),
            machine_name: None,
        },
        footprint: vec!["file:refused.txt".into()],
        reservation_id: "reservation-fixture".into(),
        reservation_expires_at: (Utc::now() + Duration::minutes(30)).to_rfc3339(),
        phase: ExecutionClaimPhase::Running,
        repair: None,
    };
    rusqlite::Connection::open(fixture.global.join("orbit.db"))
        .unwrap()
        .execute(
            "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at)
             VALUES (?1, 'distributed-execution-claim-v1', ?2, ?3, 'fixture', ?4)",
            rusqlite::params![
                fixture.workspace_id(),
                claim.claim_id,
                serde_json::to_string(&claim).unwrap(),
                Utc::now().to_rfc3339()
            ],
        )
        .unwrap();
    let drain = fixture.drain();

    for pass in 1..=3 {
        let output = classify(&fixture.runtime, &drain, 5);
        assert!(
            admitted(&output).is_empty(),
            "pass {pass}: the section refuses the claimed file: {output:#}"
        );
        assert_eq!(
            fixture.reserved(&fixture.runtime),
            Vec::<String>::new(),
            "pass {pass}: no reservation for the refused {refused}"
        );
    }

    // The same pass reserves a candidate the section admits, once.
    let admitted_task = fixture.task("admitted", TaskPriority::Medium, TaskComplexity::Low);
    for pass in 1..=2 {
        let output = classify(&fixture.runtime, &drain, 5);
        assert_eq!(admitted(&output), vec![admitted_task.clone()], "{output:#}");
        assert_eq!(
            fixture.reserved(&fixture.runtime),
            vec![admitted_task.clone()],
            "pass {pass}: the admitted task holds one reservation"
        );
    }
}

/// Memory fits by a high quantile of the deliveries' summed memory, not the
/// sum of their p90s. Of ten three-minute `low` deliveries, two used 9 GiB
/// and the rest 1: a p90 of 9 GiB over a mean of 2.6. A candidate beside two
/// running deliveries priced the same makes 27 GiB of p90s, over the 24 GiB
/// threshold even before the host's own use, yet the quantile of their sum,
/// about 16.9 GiB, fits over 2 GiB in use and is admitted. Over 10 GiB in
/// use it does not, and the candidate is refused for memory.
fn memory_fits_by_the_quantile_of_the_sum_not_the_sum_of_p90s() {
    let fixture = Fixture::new("enforce", "");
    for index in 0..10 {
        let memory = if index < 2 { 9 * GIB } else { GIB };
        fixture.finished_delivery_using("low", &[0.5; 3], memory);
    }
    fixture.running_hard_delivery();
    fixture.running_hard_delivery();
    let candidate = fixture.task("candidate", TaskPriority::High, TaskComplexity::Low);
    let drain = fixture.drain();

    write_proc(&fixture.proc_root, 10);
    let busy = classify(&fixture.runtime, &drain, 4);
    assert_eq!(admitted(&busy), Vec::<String>::new(), "{busy:#}");
    let record = recorded(&fixture.runtime, &drain, &busy);
    let [skip] = record.skipped.as_slice() else {
        panic!("{record:#?}");
    };
    assert_eq!(skip.task_id, candidate);
    assert_eq!(skip.resource.as_deref(), Some("memory"));
    assert_eq!(skip.detail, "memory 84% at +0m", "{record:#?}");

    write_proc(&fixture.proc_root, 2);
    let roomy = classify(&fixture.runtime, &drain, 4);
    assert_eq!(admitted(&roomy), vec![candidate.clone()], "{roomy:#}");
    let record = recorded(&fixture.runtime, &drain, &roomy);
    assert!(record.skipped.is_empty(), "{record:#?}");
    let [admission] = record.admitted.as_slice() else {
        panic!("{record:#?}");
    };
    assert_eq!(
        admission.idle_host, None,
        "it fits beside the running deliveries, not by the idle-host rule"
    );
    assert_eq!(fixture.reserved(&fixture.runtime), vec![candidate]);
}

/// `shadow` admits what `off` admits for the same inputs, records the
/// decisions `enforce` would have made, and reserves nothing. `off` builds
/// no forecast and records no block.
fn shadow_admits_as_off_and_records_what_enforce_would_do() {
    let fixture = Fixture::new("off", "");
    fixture.seed_profiles();
    fixture.running_hard_delivery();
    let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
    let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
    let drain = fixture.drain();

    let off = classify(&fixture.runtime, &drain, 4);
    assert_eq!(
        admitted(&off),
        vec![heavy.clone(), light.clone()],
        "{off:#}"
    );
    assert_eq!(off["admission_forecast"], Value::Null, "{off:#}");
    let off_pass = fixture
        .runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .unwrap();
    assert!(off_pass.admission_forecast.is_none());

    let shadow_runtime = fixture.with_mode("shadow", "");
    let shadow = classify(&shadow_runtime, &drain, 4);
    assert_eq!(admitted(&shadow), admitted(&off), "{shadow:#}");
    let record = recorded(&shadow_runtime, &drain, &shadow);
    assert_eq!(record.mode, "shadow");
    assert_eq!(
        ids(record.skipped.iter().map(|s| s.task_id.clone())),
        vec![heavy.clone()]
    );
    assert_eq!(record.skipped[0].detail, "cpu 120% at +1m");
    assert_eq!(
        ids(record.admitted.iter().map(|a| a.task_id.clone())),
        vec![light]
    );
    let shadow_pass = shadow_runtime
        .read_run_state(&drain)
        .unwrap()
        .unwrap()
        .pull_drain
        .drain_last_pass
        .unwrap();
    assert!(
        shadow_pass.deferred.iter().all(|w| w.task_id != heavy),
        "shadow leaves nothing waiting on gap fit: {shadow_pass:#?}"
    );
    assert!(
        fixture.reserved(&shadow_runtime).is_empty(),
        "shadow writes no reservation"
    );
}

/// Explicit per-run modes override the host without changing another drain,
/// and continue deciding identically after the host runtime is reopened.
fn per_run_mode_overrides_the_host_and_survives_reopen() {
    for mode in ["off", "shadow", "enforce"] {
        let fixture = Fixture::new("shadow", "");
        fixture.seed_profiles();
        fixture.running_hard_delivery();
        let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
        let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
        let drain = running_run(
            &fixture.runtime,
            "workspace_auto_pipeline",
            json!({"auto_concurrency":mode,"auto_concurrency_source":"explicit"}),
        );
        let other = fixture.drain();
        let config = std::fs::read(fixture.global.join("config.toml")).unwrap();
        let reopened = fixture.with_mode("shadow", "");
        let input = reopened.show_job_run(&drain).unwrap().input.unwrap();
        let pass = crate::scenarios::with_immutable_host_load(|| {
            reopened.run_deterministic(
            "classify_workspace_auto_tasks", &json!({}),
            &json!({"run_id":drain,"auto_concurrency":input["auto_concurrency"],"max_active_leaf_runs":4}),
            ToolContext::default()).unwrap()
        });
        let expected = if mode == "enforce" {
            vec![light.clone()]
        } else {
            vec![heavy.clone(), light.clone()]
        };
        assert_eq!(admitted(&pass), expected, "{mode}: {pass:#}");
        if mode == "off" {
            assert!(pass["admission_forecast"].is_null());
        } else {
            assert_eq!(recorded(&reopened, &drain, &pass).mode, mode);
        }
        assert_eq!(reopened.run_auto_concurrency(&drain).unwrap()["mode"], mode);
        assert_eq!(
            reopened.run_auto_concurrency(&other).unwrap()["mode"],
            "shadow"
        );
        assert_eq!(
            std::fs::read(fixture.global.join("config.toml")).unwrap(),
            config
        );
    }
}

/// Inherited local modes follow host edits; explicit modes continue to win.
fn a_live_local_drain_refreshes_only_its_inherited_mode() {
    for explicit in ["off", "shadow"] {
        let fixture = Fixture::new("shadow", "");
        fixture.seed_profiles();
        fixture.running_hard_delivery();
        let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
        let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
        let drain = fixture.drain();
        let pass = |mode: &str, source: &str| {
            crate::scenarios::with_immutable_host_load(|| {
                fixture
                    .runtime
                    .run_deterministic(
                        "classify_workspace_auto_tasks",
                        &json!({}),
                        &json!({"run_id": drain, "auto_concurrency": mode,
                    "auto_concurrency_source": source, "max_active_leaf_runs": 4}),
                        ToolContext::default(),
                    )
                    .unwrap()
            })
        };
        assert_eq!(
            admitted(&pass("shadow", "configured")),
            vec![heavy.clone(), light.clone()]
        );
        let path = fixture.global.join("config.toml");
        let config = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            &path,
            config.replace("mode = \"shadow\"", "mode = \"enforce\""),
        )
        .unwrap();
        assert_eq!(
            admitted(&pass(explicit, "explicit")),
            vec![heavy, light.clone()]
        );
        assert_eq!(admitted(&pass("shadow", "configured")), vec![light]);
    }
}

/// `max_active_leaf_runs` is enforced before gap fit: with one free slot,
/// two candidates that both fit admit one, and the other is queued, not
/// skipped. With the slot taken, the next pass builds no forecast and counts
/// as `ceiling_full`.
fn the_ceiling_holds_before_gap_fit() {
    let fixture = Fixture::new("enforce", "");
    fixture.seed_profiles();
    let first = fixture.task("first", TaskPriority::High, TaskComplexity::Low);
    fixture.task("second", TaskPriority::Medium, TaskComplexity::Low);
    let drain = fixture.drain();

    let pass = classify(&fixture.runtime, &drain, 1);
    assert_eq!(admitted(&pass), vec![first.clone()], "{pass:#}");
    let record = recorded(&fixture.runtime, &drain, &pass);
    assert_eq!(
        ids(record.admitted.iter().map(|a| a.task_id.clone())),
        vec![first]
    );
    assert!(record.skipped.is_empty(), "{record:#?}");

    fixture.running_hard_delivery();
    let full = classify(&fixture.runtime, &drain, 1);
    assert_eq!(admitted(&full), Vec::<String>::new(), "{full:#}");
    assert!(
        recorded(&fixture.runtime, &drain, &full)
            .forecast_at
            .is_none(),
        "a full ceiling builds no forecast"
    );
    let counted = counters(&fixture.runtime, &drain);
    assert_eq!(counted.passes, 2, "{counted:?}");
    assert_eq!(
        counted.outcome(DrainPassOutcome::Admitted),
        1,
        "{counted:?}"
    );
    assert_eq!(
        counted.outcome(DrainPassOutcome::CeilingFull),
        1,
        "{counted:?}"
    );
}

/// Sustained pressure holds the pass whatever the forecast says: nothing is
/// admitted, and no forecast is built for a pass that cannot admit.
fn the_throttle_holds_enforce_whatever_the_forecast_says() {
    let fixture = Fixture::new("enforce", "");
    fixture.seed_profiles();
    fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
    let drain = fixture.drain();

    fixture.probe.sustain_memory(&fixture.runtime, 95.0);
    let pass = classify(&fixture.runtime, &drain, 4);
    assert_eq!(admitted(&pass), Vec::<String>::new(), "{pass:#}");
    assert!(pass["resource_throttle"].is_object(), "{pass:#}");
    assert_eq!(pass["sleep_seconds"], 7, "{pass:#}");
    let record = recorded(&fixture.runtime, &drain, &pass);
    assert!(record.forecast_at.is_none(), "{record:#?}");
    assert!(record.admitted.is_empty() && record.skipped.is_empty());
    assert!(fixture.reserved(&fixture.runtime).is_empty());
}

/// A host with no profiles admits in `enforce` exactly what `off` admits.
fn a_host_without_profiles_admits_in_enforce_as_off_does() {
    let fixture = Fixture::new("off", "");
    let first = fixture.task("first", TaskPriority::High, TaskComplexity::Hard);
    let second = fixture.task("second", TaskPriority::Medium, TaskComplexity::Medium);
    let drain = fixture.drain();

    let off = classify(&fixture.runtime, &drain, 4);
    let enforce_runtime = fixture.with_mode("enforce", "");
    let enforce = classify(&enforce_runtime, &drain, 4);
    assert_eq!(admitted(&off), vec![first.clone(), second.clone()]);
    assert_eq!(admitted(&enforce), admitted(&off), "{enforce:#}");
    let record = recorded(&enforce_runtime, &drain, &enforce);
    assert!(record.skipped.is_empty(), "{record:#?}");
    assert!(
        record
            .admitted
            .iter()
            .all(|admission| admission.profile_source == "none"),
        "{record:#?}"
    );
}

/// Once the head has failed gap fit for longer than `head_max_wait_minutes`
/// (zero here, so from its second failing pass), nothing behind it is
/// admitted, even work that fits, and the drain polls rather than idles.
/// When the head fits it is admitted and the guard lifts.
fn a_starving_head_holds_backfill_until_it_fits() {
    let fixture = Fixture::new("enforce", "head_max_wait_minutes = 0\n");
    fixture.seed_profiles();
    let running = fixture.running_hard_delivery();
    let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
    let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
    let later = fixture.task("later", TaskPriority::Low, TaskComplexity::Low);
    let drain = fixture.drain();

    let first = classify(&fixture.runtime, &drain, 5);
    assert_eq!(
        admitted(&first),
        vec![light.clone(), later.clone()],
        "{first:#}"
    );
    let head = recorded(&fixture.runtime, &drain, &first).head.unwrap();
    assert_eq!(head.task_id, heavy);
    assert!(!head.protected, "it has not waited yet");

    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = classify(&fixture.runtime, &drain, 5);
    assert_eq!(admitted(&second), Vec::<String>::new(), "{second:#}");
    assert_eq!(
        second["sleep_seconds"], 7,
        "a held backlog polls: {second:#}"
    );
    let record = recorded(&fixture.runtime, &drain, &second);
    let protected = record.head.as_ref().unwrap();
    assert_eq!(protected.task_id, heavy);
    assert_eq!(protected.failing_since, head.failing_since);
    assert!(protected.protected, "{record:#?}");
    let held = record
        .skipped
        .iter()
        .find(|skip| skip.task_id == light)
        .expect("the light task is held");
    assert!(held.resource.is_none(), "{held:#?}");
    assert!(held.detail.contains(&heavy), "{held:#?}");

    // The running delivery ends: the head fits, and work behind it is
    // admitted again.
    fixture.ended(&running);
    let third = classify(&fixture.runtime, &drain, 5);
    assert_eq!(admitted(&third)[0], heavy, "{third:#}");
    assert!(admitted(&third).contains(&light), "{third:#}");
    assert!(recorded(&fixture.runtime, &drain, &third).head.is_none());
}

/// When gap fit refuses every candidate while the backlog has work, the pass
/// waits the short poll, not the idle wait, and counts as `gap_fit_held`.
fn a_pass_gap_fit_refuses_entirely_polls() {
    let fixture = Fixture::new("enforce", "");
    fixture.seed_profiles();
    fixture.running_hard_delivery();
    let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
    let drain = fixture.drain();

    let pass = classify(&fixture.runtime, &drain, 4);
    assert_eq!(admitted(&pass), Vec::<String>::new(), "{pass:#}");
    assert_eq!(pass["idle"], true, "{pass:#}");
    assert_eq!(pass["sleep_seconds"], 7, "{pass:#}");
    let record = recorded(&fixture.runtime, &drain, &pass);
    assert_eq!(record.skipped[0].task_id, heavy);
    let _: DateTime<Utc> = record.forecast_at.unwrap();
    let counted = counters(&fixture.runtime, &drain);
    assert_eq!(counted.passes, 1, "{counted:?}");
    assert_eq!(
        counted.outcome(DrainPassOutcome::GapFitHeld),
        1,
        "{counted:?}"
    );
    assert_eq!(
        counted.waiting_by_reason.get("gap_fit"),
        Some(&1),
        "{counted:?}"
    );
}

/// `shadow` and `enforce` passes each record the forecast at the offsets
/// inside the horizon (10 minutes here, so +5 and +10), counting what the
/// pass admitted, with the pass's decisions; `off` records nothing. The
/// accuracy report counts, per drain, what shadow would have held and what
/// enforce admitted or held.
fn shadow_and_enforce_passes_record_their_forecast_and_off_records_none() {
    let fixture = Fixture::new("off", "");
    fixture.finished_delivery("hard", &[1.0, 1.0, 1.0, 6.0, 1.0]);
    fixture.finished_delivery("medium", &[1.0, 6.0, 1.0]);
    // Long enough to still run at +5.
    fixture.finished_delivery("low", &[0.5; 8]);
    fixture.running_hard_delivery();
    let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
    let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
    let drain = fixture.drain();
    let store = fixture.runtime.sqlite_store().unwrap();
    let since = Utc::now() - Duration::hours(1);

    let off = classify(&fixture.runtime, &drain, 4);
    assert_eq!(admitted(&off), vec![heavy.clone(), light.clone()]);
    assert!(store.admission_forecasts(since).unwrap().is_empty());

    let shadow = classify(&fixture.with_mode("shadow", ""), &drain, 4);
    assert_eq!(admitted(&shadow), admitted(&off), "{shadow:#}");
    let enforce = classify(&fixture.with_mode("enforce", ""), &drain, 4);
    assert_eq!(admitted(&enforce), vec![light.clone()], "{enforce:#}");

    let records = store.admission_forecasts(since).unwrap();
    let [shadow_record, enforce_record] = records.as_slice() else {
        panic!("{records:#?}");
    };
    assert_eq!(shadow_record.mode, "shadow");
    assert_eq!(shadow_record.admitted, vec![heavy.clone(), light.clone()]);
    assert_eq!(enforce_record.mode, "enforce");
    assert_eq!(enforce_record.admitted, vec![light.clone()]);
    // The running `hard` delivery is over by +5 and the CPU baseline is
    // unknown, so the CPU is the admitted `low` delivery's 0.5 cores; memory
    // is the 2 GiB in use plus its 1 GiB. `medium` is over by then too.
    let expected = vec![
        AdmissionForecastPoint {
            offset_minutes: 5,
            cpu_cores: 0.5,
            memory_bytes: 3 * GIB,
        },
        AdmissionForecastPoint {
            offset_minutes: 10,
            cpu_cores: 0.0,
            memory_bytes: 2 * GIB,
        },
    ];
    for record in [shadow_record, enforce_record] {
        assert_eq!(record.workspace_id, fixture.workspace_id());
        assert_eq!(record.run_id, drain);
        assert_eq!(record.cpu_count, Some(10));
        assert_eq!(record.memory_total_bytes, Some(32 * GIB));
        assert_eq!(record.points, expected, "{record:#?}");
        assert_eq!(record.baseline_cpu_cores, Some(0.0));
        assert_eq!(record.baseline_memory_bytes, Some(2 * GIB));
        let covered = record.covered_deliveries.as_ref().unwrap();
        for task in &record.admitted {
            assert!(
                covered.iter().any(|delivery| delivery.task_id == *task
                    && delivery.workspace_id == record.workspace_id
                    && delivery.run_ids.is_empty()),
                "every admitted task is covered: {record:#?}"
            );
        }
        assert!(
            covered.iter().any(|delivery| !delivery.run_ids.is_empty()),
            "running deliveries remain covered: {record:#?}"
        );
        assert_eq!(record.fit_admitted, vec![light.clone()]);
        assert_eq!(record.fit_skipped, vec![heavy.clone()]);
    }

    let report = fixture.runtime.forecast_accuracy().unwrap();
    assert_eq!(report.forecasts, 2);
    let [decisions] = report.drains.as_slice() else {
        panic!("{report:#?}");
    };
    assert_eq!(decisions.run_id, drain);
    assert_eq!((decisions.shadow_passes, decisions.enforce_passes), (1, 1));
    assert_eq!(decisions.admitted, 2);
    assert_eq!(decisions.enforce_admitted, 1);
    assert_eq!(decisions.gap_held, vec![heavy.clone()]);
    assert_eq!(decisions.pull_held, 0);
    assert_eq!(decisions.would_hold, vec![heavy]);
    assert!(decisions.would_add.is_empty(), "{decisions:#?}");
    // No worker has measured the host yet, so nothing pairs.
    assert!(report.pairs.is_empty());
    let unmeasured: Vec<(u32, u64)> = report
        .offsets
        .iter()
        .map(|offset| (offset.offset_minutes, offset.unmeasured))
        .collect();
    assert_eq!(unmeasured, vec![(5, 2), (10, 2), (20, 0), (30, 0)]);
}

/// Each `shadow` and `enforce` pass keeps its forecast's first hour as 13
/// five-minute bucket maxima of its share of the host, on the record, the
/// classifier's output and readiness alike. The running `hard` delivery is
/// two minutes in, so its one-minute 7-core spike lands at +22m, inside the
/// +25m bucket, whose point is the CPU peak; memory is flat at the 2 GiB in
/// use plus its 1 GiB until it ends, so its peak is minute 0.
fn a_pass_records_its_forecast_hour_as_bucket_maxima() {
    let horizon = "horizon_cap_minutes = 60\n";
    let fixture = Fixture::new("off", horizon);
    let mut hard = vec![1.0; 40];
    hard[24] = 7.0;
    fixture.finished_delivery("hard", &hard);
    fixture.finished_delivery("low", &[0.5, 0.5]);
    fixture.running_hard_delivery();
    let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
    let drain = fixture.drain();

    // The delivery runs to +37m, inside the bucket ending at +40m.
    let mut cpu = vec![10.0; 13];
    cpu[5] = 70.0;
    cpu[9..].fill(0.0);
    let mut memory = vec![3.0 / 32.0 * 100.0; 13];
    memory[9..].fill(2.0 / 32.0 * 100.0);
    for mode in ["shadow", "enforce"] {
        let runtime = fixture.with_mode(mode, horizon);
        let pass = classify(&runtime, &drain, 4);
        assert_eq!(admitted(&pass), vec![light.clone()], "{mode}: {pass:#}");
        let record = recorded(&runtime, &drain, &pass);
        assert_eq!(record.mode, mode);
        let series = record
            .series
            .as_ref()
            .expect("a measured host keeps the hour");
        assert_eq!(series.bucket_minutes, 5);
        for (resource, points, expected) in [
            ("cpu", &series.cpu_percent, &cpu),
            ("memory", &series.memory_percent, &memory),
        ] {
            assert_eq!(points.len(), 13, "{mode} {resource}: {series:#?}");
            for (point, want) in points.iter().zip(expected.iter()) {
                assert!(
                    (point - want).abs() < 1e-9,
                    "{mode} {resource}: {points:?} != {expected:?}"
                );
            }
        }
        for (peak, points) in [
            (record.cpu_peak.as_ref().unwrap(), &series.cpu_percent),
            (record.memory_peak.as_ref().unwrap(), &series.memory_percent),
        ] {
            assert!(peak.minute <= 60, "{record:#?}");
            let highest = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            assert_eq!(Some(highest), peak.percent, "{record:#?}");
            let bucket = peak.minute.div_ceil(5) as usize;
            assert_eq!(points[bucket], highest, "{record:#?}");
        }
        assert_eq!(record.cpu_peak.as_ref().unwrap().minute, 22);
        assert_eq!(record.memory_peak.as_ref().unwrap().minute, 0);

        let readiness = runtime
            .workspace_auto_readiness(&[], Some(4), 50, &[])
            .unwrap();
        assert_eq!(
            readiness["capacity"]["admission_forecast"]["series"],
            pass["admission_forecast"]["series"],
            "{mode}: {readiness:#}"
        );
    }
}

/// A candidate that cannot fit under the thresholds beside running work is
/// refused while that work runs, with the minute and resource, and admitted
/// once the host holds no running delivery or reservation, with the miss it
/// was admitted over: CPU above 8 cores on its own, memory above 24 GiB on
/// its own, and memory that fits on its own but not over the 2 GiB the host
/// already uses.
#[test]
fn a_candidate_that_cannot_fit_beside_running_work_is_admitted_on_an_idle_host() {
    if !isolated(
        "dispatch_admission::gap_fit::a_candidate_that_cannot_fit_beside_running_work_is_admitted_on_an_idle_host",
    ) {
        return;
    }
    let cases: [(&[f64], u64, &str, &str); 3] = [
        (
            &[1.0, 9.0, 1.0],
            GIB,
            "cpu 90% at +1m",
            "cpu 90% at +1m on its own; admitted on an idle host",
        ),
        (
            &[1.0, 1.0, 1.0],
            25 * GIB,
            "memory 162% at +0m",
            "memory 78% at +0m on its own; admitted on an idle host",
        ),
        (
            &[1.0, 1.0, 1.0],
            23 * GIB,
            "memory 150% at +0m",
            "memory 78% at +0m with the host baseline; admitted on an idle host",
        ),
    ];
    for (cpu, memory, refused, admitted_over) in cases {
        let fixture = Fixture::new("enforce", "");
        // The running `hard` delivery borrows this profile too, as the
        // heaviest of its class: one core and `memory` at +0m.
        fixture.finished_delivery_using("medium", cpu, memory);
        let running = fixture.running_hard_delivery();
        let big = fixture.task("big", TaskPriority::High, TaskComplexity::Medium);
        let drain = fixture.drain();

        let busy = classify(&fixture.runtime, &drain, 4);
        assert_eq!(admitted(&busy), Vec::<String>::new(), "{busy:#}");
        let record = recorded(&fixture.runtime, &drain, &busy);
        let [skip] = record.skipped.as_slice() else {
            panic!("{record:#?}");
        };
        assert_eq!(skip.task_id, big);
        assert_eq!(skip.detail, refused, "{record:#?}");
        assert!(skip.resource.is_some() && skip.minute.is_some());

        fixture.ended(&running);
        let idle = classify(&fixture.runtime, &drain, 4);
        assert_eq!(admitted(&idle), vec![big.clone()], "{idle:#}");
        let record = recorded(&fixture.runtime, &drain, &idle);
        let [admission] = record.admitted.as_slice() else {
            panic!("{record:#?}");
        };
        assert_eq!(admission.idle_host.as_deref(), Some(admitted_over));
        assert!(record.head.is_none(), "{record:#?}");
        assert_eq!(fixture.reserved(&fixture.runtime), vec![big]);
    }
}

/// A head too big to fit beside running work, once protected, holds a
/// smaller candidate that fits while that work runs. On the first pass after
/// the host goes idle the head is admitted and the guard lifts; the smaller
/// candidate is then judged against the head's reservation, not held behind
/// it.
#[test]
fn a_protected_head_too_big_for_a_busy_host_is_admitted_once_the_host_is_idle() {
    if !isolated(
        "dispatch_admission::gap_fit::a_protected_head_too_big_for_a_busy_host_is_admitted_once_the_host_is_idle",
    ) {
        return;
    }
    let fixture = Fixture::new("enforce", "head_max_wait_minutes = 0\n");
    fixture.finished_delivery("hard", &[1.0, 9.0, 1.0]);
    fixture.finished_delivery("low", &[0.5, 0.5]);
    let running = fixture.running_hard_delivery();
    let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Hard);
    let light = fixture.task("light", TaskPriority::Medium, TaskComplexity::Low);
    let drain = fixture.drain();

    let first = classify(&fixture.runtime, &drain, 2);
    assert_eq!(admitted(&first), vec![light.clone()], "{first:#}");
    let head = recorded(&fixture.runtime, &drain, &first).head.unwrap();
    assert_eq!(head.task_id, heavy);

    std::thread::sleep(std::time::Duration::from_millis(20));
    let second = classify(&fixture.runtime, &drain, 2);
    assert_eq!(admitted(&second), Vec::<String>::new(), "{second:#}");
    let record = recorded(&fixture.runtime, &drain, &second);
    assert!(record.head.as_ref().unwrap().protected, "{record:#?}");
    let held = record
        .skipped
        .iter()
        .find(|skip| skip.task_id == light)
        .expect("the light task is held");
    assert!(held.resource.is_none(), "{held:#?}");

    // The light delivery the first pass admitted runs and ends, and so does
    // the running `hard` one: nothing is left to make room by finishing.
    fixture.delivered(&light);
    fixture.ended(&running);
    let third = classify(&fixture.runtime, &drain, 2);
    assert_eq!(admitted(&third), vec![heavy.clone()], "{third:#}");
    let record = recorded(&fixture.runtime, &drain, &third);
    assert!(record.head.is_none(), "the guard lifts: {record:#?}");
    assert!(record.admitted[0].idle_host.is_some(), "{record:#?}");
    let refused = record
        .skipped
        .iter()
        .find(|skip| skip.task_id == light)
        .expect("the light task is judged");
    assert_eq!(refused.detail, "cpu 95% at +1m", "{record:#?}");
}

/// The idle-host rule admits one candidate per idle forecast: in one pass the
/// first of two candidates too big for the thresholds is admitted and the
/// second is judged against it. `shadow` admits what `off` admits, records
/// the same decisions and reserves nothing.
#[test]
fn an_idle_host_admits_one_candidate_too_big_to_fit_per_pass_in_every_mode() {
    if !isolated(
        "dispatch_admission::gap_fit::an_idle_host_admits_one_candidate_too_big_to_fit_per_pass_in_every_mode",
    ) {
        return;
    }
    let fixture = Fixture::new("off", "");
    fixture.finished_delivery("hard", &[1.0, 9.0, 1.0]);
    let first = fixture.task("first", TaskPriority::High, TaskComplexity::Hard);
    let second = fixture.task("second", TaskPriority::Medium, TaskComplexity::Hard);
    let drain = fixture.drain();

    let off = classify(&fixture.runtime, &drain, 4);
    assert_eq!(admitted(&off), vec![first.clone(), second.clone()]);

    let shadow_runtime = fixture.with_mode("shadow", "");
    let shadow = classify(&shadow_runtime, &drain, 4);
    assert_eq!(admitted(&shadow), admitted(&off), "{shadow:#}");
    let shadow_record = recorded(&shadow_runtime, &drain, &shadow);
    assert!(fixture.reserved(&shadow_runtime).is_empty());

    let enforce_runtime = fixture.with_mode("enforce", "");
    let enforce = classify(&enforce_runtime, &drain, 4);
    assert_eq!(admitted(&enforce), vec![first.clone()], "{enforce:#}");
    let enforce_record = recorded(&enforce_runtime, &drain, &enforce);
    assert_eq!(fixture.reserved(&enforce_runtime), vec![first.clone()]);

    for record in [shadow_record, enforce_record] {
        let [admission] = record.admitted.as_slice() else {
            panic!("{record:#?}");
        };
        assert_eq!(admission.task_id, first);
        assert_eq!(
            admission.idle_host.as_deref(),
            Some("cpu 90% at +1m on its own; admitted on an idle host")
        );
        let [skip] = record.skipped.as_slice() else {
            panic!("{record:#?}");
        };
        assert_eq!(skip.task_id, second);
        assert_eq!(skip.detail, "cpu 180% at +1m");
    }
}
/// A protected head whose miss stays put over `head_stuck_passes` is
/// reported stuck once: its record names the profile, the miss and
/// the baseline, one friction names the head, and both last only while it is
/// the head. A head whose miss improves every pass is never reported. Neither
/// changes what is admitted.
///
/// The head is a `medium` delivery beside the running `hard` one, refused on
/// memory at +0m: the host's memory in use, plus 1 GiB for each delivery,
/// over the 24 GiB threshold.
#[test]
fn a_protected_head_whose_miss_never_improves_is_reported_stuck_once() {
    if !isolated(
        "dispatch_admission::gap_fit::a_protected_head_whose_miss_never_improves_is_reported_stuck_once",
    ) {
        return;
    }
    // Memory in use per pass, in GiB, and each pass's stall count once the
    // head is protected (from the second pass).
    let cases = [
        (
            "constant miss",
            "enforce",
            3,
            [26; 5],
            [1, 2, 3, 4],
            [0, 0, 0, 1, 1],
            vec![],
        ),
        (
            "improving miss",
            "enforce",
            3,
            [27, 26, 25, 24, 23],
            [1, 1, 1, 1],
            [0; 5],
            vec![],
        ),
        (
            "threshold one retry",
            "enforce",
            1,
            [26; 5],
            [1, 3, 4, 5],
            [0, 1, 1, 1, 1],
            vec![1],
        ),
        (
            "progress reset retry",
            "enforce",
            1,
            [27, 27, 26, 26, 25],
            [1, 1, 3, 1],
            [0, 1, 2, 2, 3],
            vec![1, 2],
        ),
        (
            "shadow stuck",
            "shadow",
            1,
            [26; 5],
            [1, 2, 3, 4],
            [0; 5],
            vec![],
        ),
    ];
    for (case, mode, threshold, used, passes, friction_counts, retries) in cases {
        let fixture = Fixture::new(
            mode,
            &format!("head_max_wait_minutes = 0\nhead_stuck_passes = {threshold}\n"),
        );
        fixture.seed_profiles();
        let running = fixture.running_hard_delivery();
        let heavy = fixture.task("heavy", TaskPriority::High, TaskComplexity::Medium);
        let drain = fixture.drain();

        let mut incident = None;
        for (pass, used_gib) in used.into_iter().enumerate() {
            write_proc(&fixture.proc_root, used_gib);
            if pass == 1 {
                // Make protection deterministic without waiting on host timing.
                let mut state = fixture.runtime.read_run_state(&drain).unwrap().unwrap();
                state
                    .pull_drain
                    .drain_last_pass
                    .as_mut()
                    .unwrap()
                    .admission_forecast
                    .as_mut()
                    .unwrap()
                    .head
                    .as_mut()
                    .unwrap()
                    .failing_since -= Duration::minutes(1);
                fixture.runtime.write_run_state(&drain, &state).unwrap();
            }
            let fault = retries.contains(&pass).then(|| {
                // Fail only after this stall's friction really lands. This hits
                // the first protected stall and a reset from a reported stall.
                let connection =
                    rusqlite::Connection::open(fixture.global.join("orbit.db")).unwrap();
                if case == "threshold one retry" {
                    connection
                        .execute_batch(&format!(
                            "CREATE TRIGGER fail_stuck_pass BEFORE UPDATE ON job_run_states \
                         WHEN NEW.run_id = '{drain}' \
                         BEGIN SELECT RAISE(ABORT, 'injected identity write failure'); END;"
                        ))
                        .unwrap();
                    let unrecorded = classify(&fixture.runtime, &drain, 4);
                    assert!(admitted(&unrecorded).is_empty(), "{case}: {unrecorded:#}");
                    assert!(
                        frictions(&fixture.runtime).is_empty(),
                        "{case}: no external friction before the stall identity is durable"
                    );
                    assert!(
                        unrecorded["admission_forecast"]["head"]["stall"]["stuck"]
                            .as_bool()
                            .unwrap()
                    );
                    assert!(
                        unrecorded["admission_forecast"]["head"]["stall"]["friction_id"].is_null()
                    );
                    connection
                        .execute_batch("DROP TRIGGER fail_stuck_pass;")
                        .unwrap();
                }
                connection
                    .execute_batch(&format!(
                        "CREATE TRIGGER fail_stuck_pass BEFORE UPDATE ON job_run_states \
                     WHEN NEW.run_id = '{drain}' AND \
                     (SELECT count(*) FROM friction_records) >= {} \
                     BEGIN SELECT RAISE(ABORT, 'injected pass-state write failure'); END;",
                        friction_counts[pass],
                    ))
                    .unwrap();
                connection
            });
            let output = classify(&fixture.runtime, &drain, 4);
            assert_eq!(
                admitted(&output),
                if mode == "shadow" {
                    vec![heavy.clone()]
                } else {
                    Vec::new()
                },
                "{case}: {output:#}"
            );
            let record: DrainAdmissionForecast =
                serde_json::from_value(output["admission_forecast"].clone()).unwrap();
            if fault.is_none() {
                recorded(&fixture.runtime, &drain, &output);
            }
            let head = record.head.as_ref().expect("the head fails gap fit");
            assert_eq!(head.task_id, heavy, "{case}");
            let miss = head.miss.as_ref().expect("the head records its miss");
            assert_eq!(
                (
                    miss.resource.as_str(),
                    miss.minute,
                    miss.profile_source.as_str()
                ),
                ("memory", 0, "exact"),
                "{case}: {record:#?}"
            );
            assert_eq!(miss.profile_key.as_deref(), Some("change/medium"), "{case}");
            let percent = (used_gib + 2) as f64 / 32.0 * 100.0;
            assert!((miss.percent - percent).abs() < 1e-9, "{case}: {record:#?}");
            let baseline = used_gib as f64 / 32.0 * 100.0;
            assert!(
                miss.baseline_percent
                    .is_some_and(|measured| (measured - baseline).abs() < 1e-9),
                "{case}: {record:#?}"
            );
            let Some(expected) = pass.checked_sub(1).map(|index| passes[index]) else {
                assert!(
                    !head.protected && head.stall.is_none(),
                    "{case}: {record:#?}"
                );
                continue;
            };
            let stall = head.stall.as_ref().expect("a protected head has a stall");
            assert_eq!(stall.passes, expected, "{case} pass {pass}: {record:#?}");
            assert_eq!(
                stall.stuck,
                expected >= threshold,
                "{case} pass {pass}: {record:#?}"
            );
            let filed = frictions(&fixture.runtime);
            assert_eq!(
                filed.len(),
                friction_counts[pass],
                "{case} pass {pass}: {filed:#?}"
            );
            if stall.stuck && mode == "enforce" {
                let friction = filed
                    .iter()
                    .find(|friction| Some(&friction.id) == stall.friction_id.as_ref())
                    .expect("the output names an actually filed friction");
                if let Some((since, id)) = &incident {
                    assert_eq!(
                        stall.friction_id.as_ref() == Some(id),
                        stall.since == *since,
                        "{case}: only distinct stalls get distinct frictions"
                    );
                }
                incident = Some((stall.since, friction.id.clone()));
                let body = &friction.body;
                for named in [heavy.as_str(), "change/medium", "exact", "memory"] {
                    assert!(body.contains(named), "{case}: {named} missing: {body}");
                }
            } else {
                assert!(stall.friction_id.is_none(), "{case}: {record:#?}");
            }
            if let Some(connection) = fault {
                let persisted = fixture.runtime.read_run_state(&drain).unwrap().unwrap();
                let persisted_head = persisted
                    .pull_drain
                    .drain_last_pass
                    .unwrap()
                    .admission_forecast
                    .unwrap()
                    .head
                    .unwrap();
                assert_ne!(
                    persisted_head
                        .stall
                        .as_ref()
                        .and_then(|stall| stall.friction_id.as_ref()),
                    stall.friction_id.as_ref(),
                    "{case}: the friction-id write really failed"
                );
                assert_eq!(
                    persisted_head.stall.as_ref().map(|stall| stall.since),
                    Some(stall.since),
                    "{case}: the identity survived even though the friction-id write failed"
                );
                connection
                    .execute_batch("DROP TRIGGER fail_stuck_pass;")
                    .unwrap();
                let retry = classify(&fixture.runtime, &drain, 4);
                assert!(admitted(&retry).is_empty(), "{case}: {retry:#}");
                assert_eq!(
                    frictions(&fixture.runtime).len(),
                    friction_counts[pass],
                    "{case}: retry must reuse the successful friction after the failed pass-state write"
                );
                let retried = recorded(&fixture.runtime, &drain, &retry);
                let retried_stall = retried.head.unwrap().stall.unwrap();
                assert_eq!(retried_stall.since, stall.since, "{case}");
                assert_eq!(retried_stall.friction_id, stall.friction_id, "{case}");
                assert_eq!(retried_stall.passes, stall.passes + 1, "{case}");
                assert!(retried_stall.stuck, "{case}");
            }
        }

        // The running delivery ends: the head is admitted, and the stuck
        // record goes with it. The friction stays as the incident's record.
        fixture.ended(&running);
        let output = classify(&fixture.runtime, &drain, 4);
        if mode == "shadow" {
            // The ended delivery's task is still backlog in this fixture, so
            // off/shadow can select it alongside the heavy head again.
            assert!(admitted(&output).contains(&heavy), "{case}: {output:#}");
            assert_eq!(admitted(&output).len(), 2, "{case}: {output:#}");
        } else {
            assert_eq!(admitted(&output), vec![heavy.clone()], "{case}: {output:#}");
        }
        assert!(recorded(&fixture.runtime, &drain, &output).head.is_none());
        assert_eq!(
            frictions(&fixture.runtime).len(),
            friction_counts[4],
            "{case}"
        );
    }
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    enforce_skips_a_colliding_candidate_and_admits_a_lighter_one_behind_it_scenarios,
    enforce_skips_a_colliding_candidate_and_admits_a_lighter_one_behind_it,
    candidates_admitted_in_one_pass_count_each_other,
    a_candidate_the_section_refuses_leaves_no_reservation,
    memory_fits_by_the_quantile_of_the_sum_not_the_sum_of_p90s,
    shadow_admits_as_off_and_records_what_enforce_would_do,
    per_run_mode_overrides_the_host_and_survives_reopen,
    a_live_local_drain_refreshes_only_its_inherited_mode,
    the_ceiling_holds_before_gap_fit,
    the_throttle_holds_enforce_whatever_the_forecast_says,
    a_host_without_profiles_admits_in_enforce_as_off_does
);

crate::scenario_table!(
    a_starving_head_holds_backfill_until_it_fits_scenarios,
    a_starving_head_holds_backfill_until_it_fits,
    a_pass_gap_fit_refuses_entirely_polls,
    shadow_and_enforce_passes_record_their_forecast_and_off_records_none,
    a_pass_records_its_forecast_hour_as_bucket_maxima
);
