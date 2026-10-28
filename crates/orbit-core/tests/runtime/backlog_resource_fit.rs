//! Resource fit in the backlog forecast [ORB-18689]: where the drain enforces
//! gap fit, heavy-profile work drains with less parallelism than light work
//! at the same ceiling, and a profile larger than the host still runs on an
//! idle one.

use chrono::{Duration, Utc};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, ShipMode, WorkspaceRuntimeBinding};
use orbit_store::contracts::{
    InvocationInsertParams, ResourceUsageStoreBackend, RunResourceMinute,
};
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use orbit_types::workflow::{JobRun, JobRunState};
use serde_json::{Value, json};

const GIB: u64 = 1024 * 1024 * 1024;
/// The fixture host's cores; `admit_cpu_percent` defaults to 90% of them.
const CPUS: u32 = 4;

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    /// A workspace whose medium deliveries each took one hour and used
    /// `cores` throughout, with four independent medium tasks in backlog.
    fn new(cores: f64) -> Self {
        let fixture = Self {
            root: tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap(),
        };
        let proc_root = fixture.root.path().join("proc");
        std::fs::create_dir_all(&proc_root).unwrap();
        let mut stat = "cpu  100 0 0 900 0 0 0 0 0 0\n".to_string();
        for cpu in 0..CPUS {
            stat.push_str(&format!("cpu{cpu} 0 0 0 0 0 0 0 0 0 0\n"));
        }
        std::fs::write(proc_root.join("stat"), stat).unwrap();
        std::fs::write(
            proc_root.join("meminfo"),
            format!(
                "MemTotal: {} kB\nMemFree: 1024 kB\nMemAvailable: {} kB\n",
                32 * GIB / 1024,
                30 * GIB / 1024
            ),
        )
        .unwrap();
        super::fixture_template::seed_store(&fixture.global());
        let runtime = fixture.runtime("enforce");
        profile_delivery(&runtime, cores);
        for _ in 0..5 {
            delivery(&runtime);
        }
        for path in ["a", "b", "c", "d"] {
            runtime
                .add_task(TaskAddParams {
                    title: format!("Pending {path}"),
                    status: Some(TaskStatus::Backlog),
                    complexity: TaskComplexity::Medium,
                    context_files: vec![format!("file:{path}.rs")],
                    ..Default::default()
                })
                .unwrap();
        }
        fixture
    }

    fn global(&self) -> std::path::PathBuf {
        self.root.path().join("global")
    }

    fn runtime(&self, mode: &str) -> OrbitRuntime {
        let workspace = self.root.path().join("repo/.orbit");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            self.global().join("config.toml"),
            format!("[machine.auto_concurrency]\nmode = '{mode}'\nmin_profile_deliveries = 1\n"),
        )
        .unwrap();
        OrbitRuntime::from_roots_with_binding(
            &self.global(),
            &workspace,
            WorkspaceRuntimeBinding {
                logical_workspace_id: "ws_forecast_fit".into(),
                task_partition_id: "ws_forecast_fit".into(),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: self.root.path().join("repo"),
                ship_mode: ShipMode::Pr,
                base_branch: Some("agent-main".into()),
            },
        )
        .unwrap()
        .with_host_load_proc_root(self.root.path().join("proc"))
    }

    fn forecast(&self, mode: &str) -> Value {
        let mut forecast = self
            .runtime(mode)
            .backlog_forecast(Default::default(), Some(4), None)
            .unwrap();
        // This fixture isolates resource fit, without regression-fix history.
        let predictions = forecast["forecast"]["task_predictions"].take();
        forecast["forecast"] = forecast["forecast"]["pre_regression_baseline"].take();
        forecast["forecast"]["task_predictions"] = predictions;
        forecast
    }
}

/// One finished hour-long medium delivery that used `cores` and 1 GiB every
/// minute: the profile every pending task is priced by.
fn profile_delivery(runtime: &OrbitRuntime, cores: f64) {
    let store = runtime.sqlite_store().unwrap();
    let workspace = runtime.workspace_id().unwrap();
    let admitted = Utc::now() - Duration::days(2);
    let run = JobRun {
        executed_on: None,
        run_id: "jrun-profiled".into(),
        job_id: "task_pr_pipeline".into(),
        attempt: 1,
        state: JobRunState::Success,
        scheduled_at: admitted,
        started_at: Some(admitted),
        finished_at: Some(admitted + Duration::minutes(60)),
        duration_ms: None,
        created_at: admitted,
        pid: None,
        pid_start_time: None,
        input: Some(json!({"task_id": "ORB-profiled"})),
        retry_source_run_id: None,
        knowledge_metrics: None,
        resolved_crew: None,
        crew_model: None,
        steps: Vec::new(),
    };
    store
        .upsert_job_run_for_workspace(&workspace, &run, None)
        .unwrap();
    for minute in 0..60u32 {
        store
            .record_run_resource_minute(&RunResourceMinute {
                workspace_id: workspace.clone(),
                run_id: run.run_id.clone(),
                minute_index: minute,
                minute_started_at: admitted + Duration::minutes(i64::from(minute)),
                task_id: "ORB-profiled".into(),
                work_class: "change".into(),
                complexity: Some("medium".into()),
                resumed: false,
                delivery_admitted_at: Some(admitted),
                cpu_cores: cores,
                cpu_accounting: Some(orbit_store::contracts::CPU_ACCOUNTING_LINUX.into()),
                memory_bytes: GIB,
                samples: 12,
                crew_model: None,
                orbit_version: None,
            })
            .unwrap();
    }
}

/// A done medium task with one hour of recorded agent work.
fn delivery(runtime: &OrbitRuntime) {
    let task = runtime
        .add_task(TaskAddParams {
            title: "Observed delivery".into(),
            status: Some(TaskStatus::Done),
            complexity: TaskComplexity::Medium,
            ..Default::default()
        })
        .unwrap();
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: format!("delivery-{}", task.id),
            activity_id: "agent_implement".into(),
            agent: "codex".into(),
            provider: None,
            account: None,
            model: Some("gpt-6.1-sol".into()),
            task_ids: vec![task.id.clone()],
            trace: InvocationTrace {
                duration_ms: 3_600_000,
                usage: TokenUsage {
                    input: 100,
                    ..Default::default()
                },
                provider_cost_usd: Some(1.0),
                ..Default::default()
            },
        })
        .unwrap();
}

fn quantile(forecast: &Value, path: &[&str], percentile: &str) -> f64 {
    path.iter()
        .fold(&forecast["forecast"], |value, key| &value[*key])[percentile]
        .as_f64()
        .unwrap_or_else(|| panic!("{path:?} {percentile}: {}", forecast["forecast"]))
}

fn heavy_profiles_drain_with_less_parallelism_at_the_same_ceiling() {
    // Five cores is more than the host has: two never fit together, and
    // one fits only on an idle host.
    let heavy = Fixture::new(5.0).forecast("enforce");
    let light = Fixture::new(0.5).forecast("enforce");
    // Resource fit landed at model 6. Model 10 keeps that factor.
    assert_eq!(heavy["model_version"], 11);
    assert_eq!(heavy["inputs"]["admission_factors"]["resource_fit"], 1);
    for forecast in [&heavy, &light] {
        let fit = &forecast["inputs"]["resource_fit"];
        assert_eq!(fit["status"], "applied", "{fit}");
        assert_eq!(fit["mode"], "enforce");
        assert_eq!(fit["admit_cpu_percent"], 90);
        assert_eq!(fit["host"]["cpu_count"], CPUS);
        assert_eq!(fit["baseline"]["source"], "none");
        assert_eq!(forecast["inputs"]["concurrency"], 4);
        assert_eq!(forecast["coverage"]["resource_profile_missing"], 0);
        assert_eq!(forecast["coverage"]["resource_profile_fallbacks"], 0);
        assert_eq!(
            forecast["forecast"]["task_predictions"][0]["profile_source"],
            "exact"
        );
    }
    for percentile in ["p50", "p90"] {
        // Light work fills all four slots and drains in one delivery.
        assert_eq!(quantile(&light, &["time_to_drain_hours"], percentile), 1.0);
        assert_eq!(
            quantile(&light, &["effective_parallelism"], percentile),
            4.0
        );
        assert_eq!(
            quantile(&light, &["wait_hours", "resource"], percentile),
            0.0
        );
        // Heavy work runs one at a time, each admitted once the host is
        // idle, so it still drains, four times later.
        assert_eq!(quantile(&heavy, &["time_to_drain_hours"], percentile), 4.0);
        assert_eq!(
            quantile(&heavy, &["effective_parallelism"], percentile),
            1.0
        );
        // The three held tasks wait three, two and one hours for room.
        assert_eq!(
            quantile(&heavy, &["wait_hours", "resource"], percentile),
            6.0
        );
        assert_eq!(
            quantile(&heavy, &["wait_hours", "capacity"], percentile),
            0.0
        );
    }
}

fn resource_fit_applies_only_where_the_drain_enforces_it() {
    let fixture = Fixture::new(5.0);
    for mode in ["off", "shadow"] {
        let forecast = fixture.forecast(mode);
        let fit = &forecast["inputs"]["resource_fit"];
        assert_eq!(fit["status"], mode, "{fit}");
        assert_eq!(fit["mode_source"], "configured");
        assert_eq!(fit["host"], Value::Null);
        assert_eq!(
            forecast["forecast"]["task_predictions"][0]["profile_source"],
            Value::Null
        );
        // Without gap fit the ceiling alone bounds the drain.
        assert_eq!(quantile(&forecast, &["time_to_drain_hours"], "p90"), 1.0);
        assert_eq!(quantile(&forecast, &["effective_parallelism"], "p50"), 4.0);
    }
}

crate::scenario_table!(
    resource_fit,
    heavy_profiles_drain_with_less_parallelism_at_the_same_ceiling,
    resource_fit_applies_only_where_the_drain_enforces_it,
);
