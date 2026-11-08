//! Review policy changes must select their own observed delivery distribution.

use chrono::{Duration, Utc};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{OrbitRuntime, ShipMode, WorkspaceRuntimeBinding};
use orbit_engine::RuntimeHost;
use orbit_store::contracts::{InvocationInsertParams, JobRunStepParams};
use orbit_types::task::{TaskComplexity, TaskStatus};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use orbit_types::workflow::{
    JobRunState, JobTargetType, PipelineState, ReviewAdmission, ReviewBudget,
    ReviewGateAdmissionOutput, ReviewGateNotApplicable, ReviewTiming,
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
    record_admitted(runtime, task, &admission(timing), hours, activity)
}

fn record_admitted(
    runtime: &OrbitRuntime,
    task: &str,
    admitted: &ReviewAdmission,
    hours: u64,
    activity: &str,
) -> String {
    let now = Utc::now();
    let run = runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            now,
            Some(json!({"task_id": task, "review": admitted})),
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
            agent_execution_id: None,
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

fn write_gate_not_applicable(runtime: &OrbitRuntime, run_id: &str, admitted: &ReviewAdmission) {
    let output = ReviewGateAdmissionOutput::NotApplicable(ReviewGateNotApplicable::new(
        "review_before_landing_off",
        Some(admitted),
    ));
    let mut state = PipelineState::new(run_id.to_string(), "task_pr_pipeline".into(), json!({}));
    state.record_pipeline_output(
        "landing_review_gate_admit",
        serde_json::to_value(output).expect("gate output"),
    );
    runtime.write_run_state(run_id, &state).unwrap();
}

/// Plant checkpoint bytes in this fixture's own store. The preserved
/// historical checkpoint is not this database and is not modified.
fn plant_checkpoint(runtime: &OrbitRuntime, run_id: &str, bytes: &str) {
    let changed = runtime
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO job_run_states(workspace_id, run_id, pipeline_state_json) \
             SELECT workspace_id, run_id, ?1 FROM job_runs WHERE run_id = ?2 \
             ON CONFLICT(workspace_id, run_id) DO UPDATE SET pipeline_state_json = excluded.pipeline_state_json",
            rusqlite::params![bytes, run_id],
        )
        .unwrap();
    assert_eq!(changed, 1, "fixture checkpoint for {run_id}");
}

fn checkpoint_bytes(runtime: &OrbitRuntime, run_id: &str) -> String {
    runtime
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .query_row(
            "SELECT pipeline_state_json FROM job_run_states WHERE run_id = ?1",
            rusqlite::params![run_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn done_task(runtime: &OrbitRuntime, title: &str) -> String {
    runtime
        .add_task(TaskAddParams {
            title: title.into(),
            status: Some(TaskStatus::Done),
            complexity: TaskComplexity::Medium,
            crew: Some("sample".into()),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn exempt_step(runtime: &OrbitRuntime, run_id: &str) {
    let now = Utc::now();
    runtime
        .complete_job_run_step(
            run_id,
            &JobRunStepParams {
                step_index: 1,
                target_type: JobTargetType::Activity,
                target_id: "review_gate_admit".into(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(0),
                exit_code: Some(0),
                agent_response_json: Some(json!({
                    "applies": false,
                    "reason": "no_diff_exemption",
                    "timing": "before-landing",
                    "timing_source": "workspace"
                })),
                state: JobRunState::Success,
                lock_wait_ms: None,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
}

fn forecast_riskiness_preserves_observation_timestamps() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime("off");
    for _ in 0..5 {
        delivery(&runtime, ReviewTiming::None, 1);
    }
    let pending_id = pending(&runtime, TaskComplexity::Medium, "sample");
    let result = runtime
        .backlog_forecast(Default::default(), None, None)
        .unwrap();
    let predictions = result["forecast"]["task_predictions"]
        .as_array()
        .unwrap()
        .clone();
    let prediction = predictions
        .iter()
        .find(|row| row["id"] == pending_id)
        .unwrap_or_else(|| panic!("missing prediction for {pending_id}: {result}"))
        .clone();
    let riskiness = &prediction["riskiness"];
    for field in ["since", "observed_through"] {
        let raw = riskiness[field]
            .as_str()
            .unwrap_or_else(|| panic!("riskiness.{field} must be a string: {riskiness}"));
        assert!(
            raw.contains('T'),
            "riskiness.{field} must retain the RFC3339 instant, not a date-only string \
             ({raw}): {prediction}"
        );
        chrono::DateTime::parse_from_rfc3339(raw).unwrap_or_else(|_| {
            panic!("riskiness.{field} must parse as RFC3339 ({raw}): {prediction}")
        });
    }
    let since = chrono::DateTime::parse_from_rfc3339(riskiness["since"].as_str().unwrap()).unwrap();
    let observed_through =
        chrono::DateTime::parse_from_rfc3339(riskiness["observed_through"].as_str().unwrap())
            .unwrap();
    assert!(
        since < observed_through,
        "the observation window must advance: {riskiness}"
    );
    // The window is a rolling interval, not day-aligned: its length matches
    // the configured window within scan skew, not a calendar boundary.
    let window_days = riskiness["window_days"].as_u64().unwrap() as f64;
    let window_hours = (observed_through - since).num_seconds() as f64 / 3600.0;
    assert!(
        (window_hours - window_days * 24.0).abs() < 1.0,
        "the window must span window_days ({window_days}): {riskiness}"
    );
    let scan_age = (Utc::now() - observed_through.with_timezone(&Utc))
        .num_seconds()
        .abs();
    assert!(
        scan_age < 3600,
        "observed_through must be the scan time: {riskiness}"
    );
    // The stored tasks keep the same exact window the live forecast serves.
    let stored: Value =
        serde_json::from_str(&runtime.backlog_forecasts(1).unwrap()[0].tasks_json).unwrap();
    let stored_row = stored
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == pending_id)
        .unwrap_or_else(|| panic!("missing stored task for {pending_id}: {stored}"))
        .clone();
    assert_eq!(
        stored_row["riskiness"]["since"], riskiness["since"],
        "stored and live riskiness must share the estimator window: {stored_row} vs {prediction}"
    );
    assert_eq!(
        stored_row["riskiness"]["observed_through"], riskiness["observed_through"],
        "stored and live riskiness must share the estimator window: {stored_row} vs {prediction}"
    );
    // The task API reads the same estimator shape: identical fields and a
    // window from the same scan, within clock skew between the two scans.
    let task = runtime.get_task(&pending_id).unwrap();
    let estimated = runtime.task_riskiness_model().unwrap().estimate(&task);
    assert_eq!(
        estimated.window_days as u64,
        riskiness["window_days"].as_u64().unwrap(),
        "forecast and task API must share the estimator window: {estimated:?} vs {prediction}"
    );
    for (field, expected_raw) in [
        ("since", estimated.since.as_str()),
        ("observed_through", estimated.observed_through.as_str()),
    ] {
        let live = chrono::DateTime::parse_from_rfc3339(riskiness[field].as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        let expected = chrono::DateTime::parse_from_rfc3339(expected_raw)
            .unwrap()
            .with_timezone(&Utc);
        let skew = (live - expected).num_seconds().abs();
        assert!(
            skew < 3600,
            "riskiness.{field} must track the estimator scan ({expected_raw}): {prediction}"
        );
    }
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

fn drain_scope_separates_scheduled_and_no_drain_work_from_completion() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime("none");
    let delivered: Vec<_> = (0..8)
        .map(|_| delivery(&runtime, ReviewTiming::None, 1))
        .collect();
    let ready = pending(&runtime, TaskComplexity::Medium, "sample");
    let scheduled = pending(&runtime, TaskComplexity::Medium, "sample");
    let excluded = pending(&runtime, TaskComplexity::Medium, "sample");
    let not_before = Utc::now() + Duration::days(24);
    for (id, params) in [
        (
            &scheduled,
            TaskUpdateParams {
                not_before: Some(Some(not_before)),
                ..Default::default()
            },
        ),
        (
            &excluded,
            TaskUpdateParams {
                tags: Some(vec!["no-drain".into()]),
                ..Default::default()
            },
        ),
    ] {
        runtime
            .update_task_with_identity(id, params, Some("codex".into()), None)
            .unwrap();
    }
    let result = forecast(&runtime);
    let source = &result["forecast"];
    let predictions = source["task_predictions"].as_array().unwrap();
    let prediction = |id: &str| predictions.iter().find(|task| task["id"] == id).unwrap();
    for percentile in ["p50", "p90"] {
        assert_eq!(
            source["time_to_drain_hours"][percentile], 1.0,
            "only the startable task's hour belongs to time to drain: {result}"
        );
        assert_eq!(
            source["tokens"][percentile], 300.0,
            "all three tasks still cost tokens"
        );
        assert_eq!(
            source["usd"][percentile], 3.0,
            "all three tasks still cost money"
        );
        assert_eq!(source["by_complexity"][0]["work_hours"][percentile], 3.0);
        assert_eq!(source["wait_hours"]["scheduled"][percentile], 0.0);
        assert_eq!(prediction(&ready)["predicted_start_hours"][percentile], 0.0);
        for id in [&scheduled, &excluded] {
            assert!(prediction(id)["predicted_start_hours"][percentile].is_null());
            assert_eq!(prediction(id)["work_hours"][percentile], 1.0);
        }
    }
    assert_eq!(source["drain_tasks"], 1);
    assert!(
        source["schedulable_subset"].is_null(),
        "intentional exclusions are not missing coverage"
    );
    assert_eq!(prediction(&scheduled)["drain_hold"]["kind"], "scheduled");
    assert_eq!(
        prediction(&scheduled)["drain_hold"]["starts_at"],
        not_before.to_rfc3339()
    );
    assert_eq!(prediction(&excluded)["drain_hold"]["kind"], "excluded");
    assert_eq!(
        prediction(&excluded)["drain_hold"]["reason"],
        "no_drain_tag"
    );
    assert!(
        prediction(&excluded)["drain_hold"]["detail"]
            .as_str()
            .unwrap()
            .contains("no automatic drain")
    );
    // A mature snapshot can finish its drain while held work remains open.
    // Exercise the public scoring API with the same held population.
    let mature_at = Utc::now() - Duration::days(90);
    runtime.record_backlog_forecast(orbit_core::StoredBacklogForecast {
        workspace_id: String::new(),
        forecast_at: mature_at,
        model_version: result["model_version"].as_u64().unwrap() as u32,
        inputs_json: "{}".into(),
        result_json: "{}".into(),
        tasks_json: json!([
            {"id": delivered[0], "complexity": "medium", "forecast": {}},
            {"id": scheduled, "complexity": "medium", "drain_hold": prediction(&scheduled)["drain_hold"]},
            {"id": excluded, "complexity": "medium", "drain_hold": prediction(&excluded)["drain_hold"]}
        ]).to_string(),
    }, Duration::zero()).unwrap();
    let scored = runtime
        .backlog_forecast(Default::default(), Some(1), None)
        .unwrap();
    let mature = scored["calibration"]["forecasts"]
        .as_array()
        .unwrap()
        .first()
        .unwrap();
    assert_eq!(
        mature["actual"]["drained"], true,
        "held work must not hold drain-time actuals open: {mature}"
    );
    assert!(mature["actual"]["time_to_drain_hours"].as_f64().unwrap() <= 2160.0);

    // Canonical persistence must retain the drain population for later scoring.
    runtime
        .backlog_forecast(Default::default(), None, None)
        .unwrap();
    let stored: Value =
        serde_json::from_str(&runtime.backlog_forecasts(1).unwrap()[0].tasks_json).unwrap();
    let stored_rows = stored.as_array().unwrap();
    for id in [&scheduled, &excluded] {
        let row = stored_rows.iter().find(|row| row["id"] == *id).unwrap();
        assert_eq!(row["drain_hold"], prediction(id)["drain_hold"]);
    }

    // With no startable work left, held tasks still have spending estimates.
    runtime
        .update_task_with_identity(
            &ready,
            TaskUpdateParams {
                tags: Some(vec!["no-drain".into()]),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let held = runtime
        .backlog_forecast(Default::default(), Some(1), None)
        .unwrap();
    assert_eq!(held["forecast"]["drain_tasks"], 0);
    for percentile in ["p50", "p90"] {
        assert_eq!(held["forecast"]["time_to_drain_hours"][percentile], 0.0);
        assert_eq!(held["forecast"]["usd"][percentile], 3.0);
        assert_eq!(held["forecast"]["tokens"][percentile], 300.0);
    }
    assert_eq!(held["coverage"]["dependency_unforecastable"], 0);

    // A schedule in the past returns the task to the drain population.
    runtime
        .update_task_with_identity(
            &scheduled,
            TaskUpdateParams {
                not_before: Some(Some(Utc::now() - Duration::days(1))),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();
    let due = forecast(&runtime);
    assert_eq!(due["forecast"]["drain_tasks"], 1);
    assert_eq!(due["forecast"]["time_to_drain_hours"]["p50"], 1.0);
}

fn unreadable_historical_checkpoint_keeps_forecast_and_marks_review_unknown() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime("off");
    let mut quiet_runs = Vec::new();
    for _ in 0..5 {
        let task = done_task(&runtime, "Unreviewed delivery");
        quiet_runs.push(record_attempt(&runtime, &task, ReviewTiming::None, 1));
    }
    let exempt_task = done_task(&runtime, "Verified exempt delivery");
    let exempt_run = record_activity(
        &runtime,
        &exempt_task,
        ReviewTiming::BeforeLanding,
        1,
        "agent_implement",
    );
    exempt_step(&runtime, &exempt_run);
    let skipped = admission(ReviewTiming::None);
    let skipped_task = done_task(&runtime, "Gate skip");
    let skipped_run = record_admitted(&runtime, &skipped_task, &skipped, 1, "agent_implement");
    write_gate_not_applicable(&runtime, &skipped_run, &skipped);
    let reviewed = done_task(&runtime, "Reviewed delivery");
    let reviewed_run = record_attempt(&runtime, &reviewed, ReviewTiming::BeforeLanding, 10);
    let empty_task = done_task(&runtime, "Empty checkpoint");
    let empty_run = record_admitted(
        &runtime,
        &empty_task,
        &admission(ReviewTiming::None),
        20,
        "agent_implement",
    );
    exempt_step(&runtime, &empty_run);
    plant_checkpoint(&runtime, &empty_run, "");
    let malformed_task = done_task(&runtime, "Malformed checkpoint");
    let malformed_admission = admission(ReviewTiming::None);
    let malformed_run = record_admitted(
        &runtime,
        &malformed_task,
        &malformed_admission,
        20,
        "agent_implement",
    );
    plant_checkpoint(&runtime, &malformed_run, "{not json");
    let pending_id = pending(&runtime, TaskComplexity::Medium, "sample");
    let running = runtime
        .add_task(TaskAddParams {
            title: "Running with an unreadable checkpoint".into(),
            status: Some(TaskStatus::InProgress),
            complexity: TaskComplexity::Medium,
            crew: Some("sample".into()),
            context_files: vec!["file:running.rs".into()],
            ..Default::default()
        })
        .unwrap()
        .id;
    let running_run = record_attempt(&runtime, &running, ReviewTiming::BeforeLanding, 0);
    plant_checkpoint(&runtime, &running_run, "");
    runtime
        .update_task_with_identity(
            &running,
            TaskUpdateParams {
                job_run_id: Some(Some(running_run.clone())),
                ..Default::default()
            },
            Some("codex".into()),
            None,
        )
        .unwrap();

    let result = runtime
        .serve_backlog_forecast(Default::default(), Some(1), None, false)
        .expect("an unreadable historical checkpoint must not abort the forecast");
    assert!(
        runtime.backlog_forecasts(1).unwrap().is_empty(),
        "a noncanonical forecast must not write the hourly snapshot"
    );
    let mut viewed = result;
    let predictions = viewed["forecast"]["task_predictions"].take();
    viewed["forecast"] = viewed["forecast"]["pre_regression_baseline"].take();
    viewed["forecast"]["task_predictions"] = predictions;
    let prediction = |id: &str| {
        viewed["forecast"]["task_predictions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("missing prediction for {id}: {viewed}"))
            .clone()
    };
    let backlog = prediction(&pending_id);
    for metric in ["work_hours", "usd"] {
        assert_eq!(
            backlog[metric]["p50"], 1.0,
            "{metric} no-review cell: {backlog}"
        );
        assert_eq!(
            backlog[metric]["p90"], 1.0,
            "{metric} no-review cell: {backlog}"
        );
    }
    assert_eq!(backlog["review_mode"], "none", "{backlog}");
    let running_row = prediction(&running);
    assert_eq!(running_row["review_mode"], "unknown", "{running_row}");
    assert_eq!(
        running_row["review_mode_source"], "unreadable_run_checkpoint",
        "{running_row}"
    );
    assert_eq!(
        running_row["usd"]["p90"], 20.0,
        "unreadable history keeps its cost: {running_row}"
    );
    assert_eq!(running_row["work_hours"]["p90"], 20.0, "{running_row}");
    assert!(
        running_row["usd"]["p50"].as_f64().unwrap() > 0.0,
        "{running_row}"
    );
    assert_eq!(viewed["inputs"]["history_deliveries"], 10);
    assert_eq!(viewed["inputs"]["history_review_modes"]["none"], 7);
    assert_eq!(
        viewed["inputs"]["history_review_modes"]["before_landing"],
        1
    );
    assert_eq!(viewed["inputs"]["history_review_modes"]["unknown"], 2);
    assert_eq!(viewed["coverage"]["history_review_mode_unknown"], 2);
    assert_eq!(viewed["coverage"]["review_checkpoint_unreadable"], 3);
    assert_eq!(
        viewed["proposed_coverage"]["review_checkpoint_unreadable"],
        3
    );
    let gaps = viewed["inputs"]["review_evidence_gaps"]
        .as_array()
        .expect("gaps");
    let gap_ids: Vec<_> = gaps
        .iter()
        .map(|gap| gap["run_id"].as_str().unwrap().to_string())
        .collect();
    let mut expected = vec![
        empty_run.clone(),
        malformed_run.clone(),
        running_run.clone(),
    ];
    expected.sort();
    assert_eq!(gap_ids, expected, "{gaps:?}");
    for gap in gaps {
        assert_eq!(gap["coverage_gap"], "review_checkpoint_unreadable");
        let reason = gap["reason"].as_str().unwrap();
        assert!(
            reason.starts_with("store error: invalid pipeline_state_json:"),
            "{reason}"
        );
    }
    let empty_gap = gaps.iter().find(|gap| gap["run_id"] == empty_run).unwrap();
    assert!(
        empty_gap["reason"].as_str().unwrap().contains("EOF"),
        "{empty_gap}"
    );
    for run_id in quiet_runs
        .iter()
        .chain([&exempt_run, &skipped_run, &reviewed_run])
    {
        assert!(
            !gap_ids.iter().any(|id| id == run_id),
            "readable or missing evidence is not an unreadable gap: {run_id}"
        );
    }
    assert!(runtime.read_run_state(&quiet_runs[0]).unwrap().is_none());
    assert!(runtime.read_run_state(&skipped_run).unwrap().is_some());
    for run_id in [&empty_run, &malformed_run, &running_run] {
        let error = runtime.read_run_state(run_id).unwrap_err();
        assert!(
            error.to_string().contains("invalid pipeline_state_json"),
            "{error}"
        );
    }
    let exact = runtime
        .read_run_states_exact(&[empty_run.clone(), malformed_run.clone()])
        .unwrap_err();
    assert!(
        exact.to_string().contains("invalid pipeline_state_json"),
        "{exact}"
    );
    assert_eq!(checkpoint_bytes(&runtime, &empty_run), "");
    assert_eq!(checkpoint_bytes(&runtime, &malformed_run), "{not json");
    assert_eq!(checkpoint_bytes(&runtime, &running_run), "");
}

crate::scenario_table!(
    review_modes,
    forecast_riskiness_preserves_observation_timestamps,
    drain_scope_separates_scheduled_and_no_drain_work_from_completion,
    review_policy_selects_observed_delivery_distribution,
    thin_cells_preserve_mode_before_borrowing_pooled_history,
    historical_uncertainty_and_running_admission_remain_explicit,
    unreadable_historical_checkpoint_keeps_forecast_and_marks_review_unknown,
);
