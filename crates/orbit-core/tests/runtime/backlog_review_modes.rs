//! Review policy changes must select their own observed delivery distribution.

use chrono::{Duration, Utc};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, ShipMode, WorkspaceRuntimeBinding};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{InvocationInsertParams, JobRunStepParams};
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use orbit_types::workflow::{
    JobRunState, JobTargetType, ReviewAdmission, ReviewBudget, ReviewTiming,
};
use serde_json::{Value, json};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap(),
        }
    }

    fn runtime(&self, timing: &str) -> OrbitRuntime {
        let global = self.root.path().join("global");
        let workspace = self.root.path().join("repo/.orbit");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("config.toml"),
            format!(
                "[review]\ntiming = '{timing}'\n[workflow]\ndefault_crew = 'sample'\n\
                 [workflow.purpose_crews]\nreview = 'sample'\n\
                 [crews.sample]\nprovider = 'codex'\nmodel = 'gpt-6.1-sol'\n\
                 [crews.other]\nprovider = 'codex'\nmodel = 'gpt-6.1-sol'\n"
            ),
        )
        .unwrap();
        super::fixture_template::seed_store(&global);
        OrbitRuntime::from_roots_with_binding(
            &global,
            &workspace,
            WorkspaceRuntimeBinding {
                logical_workspace_id: "ws_forecast_modes".into(),
                task_partition_id: "ws_forecast_modes".into(),
                owner_machine_id: None,
                checkout_role: None,
                repo_root: self.root.path().join("repo"),
                ship_mode: ShipMode::Pr,
                base_branch: Some("agent-main".into()),
                generation_authority: None,
            },
        )
        .unwrap()
    }
}

fn admission(timing: ReviewTiming) -> ReviewAdmission {
    ReviewAdmission {
        contract_version: 1,
        policy_version: 1,
        timing,
        timing_source: "workspace".into(),
        crew: Some("sample".into()),
        crew_source: "workspace".into(),
        crew_pool: Vec::new(),
        budget: ReviewBudget { minutes: 30 },
        required_validation_commands: Some(Vec::new()),
        baseline_commands: Vec::new(),
        host_evidence: Vec::new(),
        selective_before_landing: None,
        captured_at: Utc::now(),
    }
}

fn delivery(runtime: &OrbitRuntime, timing: ReviewTiming, hours: u64) -> String {
    let task = runtime
        .add_task(TaskAddParams {
            title: "Observed delivery".into(),
            status: Some(TaskStatus::Done),
            complexity: TaskComplexity::Medium,
            crew: Some("sample".into()),
            ..Default::default()
        })
        .unwrap();
    record_attempt(runtime, &task.id, timing, hours);
    task.id
}

fn record_attempt(runtime: &OrbitRuntime, task: &str, timing: ReviewTiming, hours: u64) -> String {
    let activity = if timing == ReviewTiming::None {
        "agent_implement"
    } else {
        "agent_review_repair"
    };
    record_activity(runtime, task, timing, hours, activity)
}

fn record_activity(
    runtime: &OrbitRuntime,
    task: &str,
    timing: ReviewTiming,
    hours: u64,
    activity: &str,
) -> String {
    let now = Utc::now();
    let run = runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            now,
            Some(json!({"task_id": task, "review": admission(timing)})),
            None,
        )
        .unwrap();
    runtime
        .complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: activity.into(),
                started_at: now - Duration::hours(hours as i64),
                finished_at: now,
                duration_ms: Some(hours * 3_600_000),
                exit_code: Some(0),
                agent_response_json: None,
                state: JobRunState::Success,
                lock_wait_ms: None,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: run.run_id.clone(),
            activity_id: activity.into(),
            agent: "codex".into(),
            provider: None,
            account: None,
            model: Some("gpt-6.1-sol".into()),
            task_ids: vec![task.into()],
            trace: InvocationTrace {
                duration_ms: hours * 3_600_000,
                usage: TokenUsage {
                    input: hours * 100,
                    ..Default::default()
                },
                provider_cost_usd: Some(hours as f64),
                ..Default::default()
            },
        })
        .unwrap();
    run.run_id
}

fn pending(runtime: &OrbitRuntime, complexity: TaskComplexity, crew: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: "Pending delivery".into(),
            status: Some(TaskStatus::Backlog),
            complexity,
            crew: Some(crew.into()),
            context_files: vec!["file:pending.rs".into()],
            ..Default::default()
        })
        .unwrap()
        .id
}

fn forecast(runtime: &OrbitRuntime) -> Value {
    let mut forecast = runtime
        .backlog_forecast(Default::default(), Some(1), None)
        .unwrap();
    // This fixture isolates review sampling, without regression-fix history.
    let predictions = forecast["forecast"]["task_predictions"].take();
    forecast["forecast"] = forecast["forecast"]["pre_regression_baseline"].take();
    forecast["forecast"]["task_predictions"] = predictions;
    forecast
}

fn review_policy_selects_observed_delivery_distribution() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime("off");
    for _ in 0..5 {
        delivery(&runtime, ReviewTiming::None, 1);
        delivery(&runtime, ReviewTiming::BeforeLanding, 10);
    }
    pending(&runtime, TaskComplexity::Medium, "sample");
    let off = forecast(&runtime);
    drop(runtime);
    let on = forecast(&fixture.runtime("before_landing"));
    assert!(
        off["forecast"]["time_to_drain_hours"]["p50"]
            .as_f64()
            .unwrap()
            < on["forecast"]["time_to_drain_hours"]["p50"]
                .as_f64()
                .unwrap(),
        "changing review policy must select fast unreviewed versus slow reviewed history: off={off}, on={on}"
    );
    for percentile in ["p50", "p90"] {
        assert_eq!(off["forecast"]["time_to_drain_hours"][percentile], 1.0);
        assert_eq!(on["forecast"]["time_to_drain_hours"][percentile], 10.0);
        assert_eq!(off["forecast"]["usd"][percentile], 1.0);
        assert_eq!(on["forecast"]["usd"][percentile], 10.0);
        assert_eq!(off["forecast"]["tokens"][percentile], 100.0);
        assert_eq!(on["forecast"]["tokens"][percentile], 1000.0);
    }
    assert_eq!(on["coverage"]["review_mode_fallbacks"], 0);
    assert_eq!(on["inputs"]["admission_factors"]["review_modes"], 1);
    assert!(on["model_version"].as_u64().unwrap() > 3);
    assert_eq!(
        on["forecast"]["task_predictions"][0]["review_mode"],
        "before_landing"
    );
    let runtime = fixture.runtime("before_pr");
    let unsupported = forecast(&runtime);
    assert_eq!(unsupported["coverage"]["review_mode_fallbacks"], 1);
    assert_eq!(
        unsupported["forecast"]["task_predictions"][0]["basis"],
        "mode_pooled_cell"
    );
    for _ in 0..5 {
        delivery(&runtime, ReviewTiming::BeforePr, 3);
    }
    let before_pr = forecast(&runtime);
    assert_eq!(before_pr["forecast"]["time_to_drain_hours"]["p90"], 3.0);
    assert_eq!(before_pr["coverage"]["review_mode_fallbacks"], 0);
}

fn thin_cells_preserve_mode_before_borrowing_pooled_history() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime("before_landing");
    for _ in 0..5 {
        delivery(&runtime, ReviewTiming::None, 1);
    }
    for _ in 0..4 {
        delivery(&runtime, ReviewTiming::BeforeLanding, 10);
    }
    pending(&runtime, TaskComplexity::Medium, "sample");
    let thin = forecast(&runtime);
    assert_eq!(thin["coverage"]["review_mode_fallbacks"], 1);
    assert_eq!(thin["forecast"]["task_predictions"][0]["cell_samples"], 4);
    assert_eq!(
        thin["forecast"]["task_predictions"][0]["basis"],
        "mode_pooled_cell"
    );
    delivery(&runtime, ReviewTiming::BeforeLanding, 10);
    let other_crew = pending(&runtime, TaskComplexity::Medium, "other");
    let other_complexity = pending(&runtime, TaskComplexity::Hard, "sample");
    let supported = forecast(&runtime);
    let prediction = |id: &str| {
        supported["forecast"]["task_predictions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap()
    };
    assert_eq!(prediction(&other_crew)["basis"], "review_complexity");
    assert_eq!(prediction(&other_complexity)["basis"], "review_workspace");
    assert_eq!(supported["coverage"]["review_mode_fallbacks"], 0);
    assert_eq!(supported["coverage"]["fallback_complexity"], 1);
    assert_eq!(supported["coverage"]["fallback_workspace"], 1);
    assert_eq!(supported["forecast"]["time_to_drain_hours"]["p90"], 30.0);
}

fn historical_uncertainty_and_running_admission_remain_explicit() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime("before_landing");
    for _ in 0..5 {
        delivery(&runtime, ReviewTiming::BeforeLanding, 10);
        delivery(&runtime, ReviewTiming::None, 1);
    }
    let mixed = delivery(&runtime, ReviewTiming::None, 1);
    record_attempt(&runtime, &mixed, ReviewTiming::BeforePr, 3);
    let contradictory = delivery(&runtime, ReviewTiming::None, 1);
    record_activity(
        &runtime,
        &contradictory,
        ReviewTiming::None,
        1,
        "agent_review_repair",
    );
    let missing = runtime
        .add_task(TaskAddParams {
            title: "Missing reviewer evidence".into(),
            status: Some(TaskStatus::Done),
            complexity: TaskComplexity::Medium,
            ..Default::default()
        })
        .unwrap()
        .id;
    record_activity(
        &runtime,
        &missing,
        ReviewTiming::BeforeLanding,
        1,
        "agent_implement",
    );
    for reason in ["no_diff_exemption", "skip_review_light_delivery"] {
        let task = runtime
            .add_task(TaskAddParams {
                title: "Verified exempt delivery".into(),
                status: Some(TaskStatus::Done),
                complexity: TaskComplexity::Medium,
                ..Default::default()
            })
            .unwrap()
            .id;
        let run = record_activity(
            &runtime,
            &task,
            ReviewTiming::BeforeLanding,
            1,
            "agent_implement",
        );
        let now = Utc::now();
        runtime.complete_job_run_step(&run, &JobRunStepParams {
            step_index: 1,
            target_type: JobTargetType::Activity,
            target_id: "review_gate_admit".into(),
            started_at: now,
            finished_at: now,
            duration_ms: Some(0),
            exit_code: Some(0),
            agent_response_json: Some(json!({"applies": false, "reason": reason, "timing": "before-landing", "timing_source": "workspace"})),
            state: JobRunState::Success,
            lock_wait_ms: None,
            error_code: None,
            error_message: None,
        }).unwrap();
    }
    let running = runtime
        .add_task(TaskAddParams {
            title: "Already running without review".into(),
            status: Some(TaskStatus::InProgress),
            complexity: TaskComplexity::Medium,
            crew: Some("sample".into()),
            context_files: vec!["file:running.rs".into()],
            ..Default::default()
        })
        .unwrap()
        .id;
    let run_id = record_attempt(&runtime, &running, ReviewTiming::None, 0);
    runtime
        .update_task_with_identity(
            &running,
            TaskUpdateParams {
                job_run_id: Some(Some(run_id)),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let current = forecast(&runtime);
    assert_eq!(current["inputs"]["history_review_modes"]["mixed"], 1);
    assert_eq!(current["inputs"]["history_review_modes"]["none"], 7);
    assert_eq!(current["coverage"]["history_review_mode_unknown"], 2);
    assert_eq!(
        current["forecast"]["task_predictions"][0]["review_mode"],
        "none"
    );
    assert_eq!(
        current["forecast"]["task_predictions"][0]["review_mode_source"],
        "captured:workspace"
    );
    assert_eq!(current["forecast"]["time_to_drain_hours"]["p90"], 1.0);
    let canonical = runtime
        .backlog_forecast(Default::default(), None, None)
        .unwrap();
    assert_eq!(canonical["stored"]["recorded"], true);
    let stored: Value =
        serde_json::from_str(&runtime.backlog_forecasts(1).unwrap()[0].tasks_json).unwrap();
    assert_eq!(stored[0]["review_mode"], "none");
    assert_eq!(stored[0]["review_mode_source"], "captured:workspace");
}

fn scheduled_forecast_starts_at_or_after_not_before() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime("none");
    for _ in 0..8 {
        delivery(&runtime, ReviewTiming::None, 1);
    }
    let id = pending(&runtime, TaskComplexity::Medium, "sample");
    let not_before = Utc::now() + Duration::hours(24);
    runtime
        .update_task_with_identity(
            &id,
            TaskUpdateParams {
                not_before: Some(Some(not_before)),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let before = Utc::now();
    let result = forecast(&runtime);
    let lower = (not_before - Utc::now()).num_seconds() as f64 / 3600.0;
    let upper = (not_before - before).num_seconds() as f64 / 3600.0 + 0.01;
    let predictions = result["forecast"]["task_predictions"].as_array().unwrap();
    let task = predictions.iter().find(|task| task["id"] == id).unwrap();
    for percentile in ["p50", "p90"] {
        let start = task["predicted_start_hours"][percentile].as_f64().unwrap();
        assert!(start >= lower && start <= upper, "{result}");
        assert!(
            result["forecast"]["time_to_drain_hours"][percentile]
                .as_f64()
                .unwrap()
                >= start + 1.0,
            "the scheduled wait must precede the one-hour delivery: {result}"
        );
    }
}

crate::scenario_table!(
    review_modes,
    scheduled_forecast_starts_at_or_after_not_before,
    review_policy_selects_observed_delivery_distribution,
    thin_cells_preserve_mode_before_borrowing_pooled_history,
    historical_uncertainty_and_running_admission_remain_explicit,
);
