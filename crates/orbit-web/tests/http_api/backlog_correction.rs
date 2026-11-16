//! Residual correction at the public forecast API.
//!
//! Delivery history is five one-hour medium tasks, so an uncorrected draw is
//! one hour. Eight older medium tasks actually took four hours against a
//! stored base of one hour. The open tasks are not that history: the newest
//! five completions are the one-hour draws.

use chrono::{DateTime, Duration, Utc};
use orbit_core::StoredBacklogForecast;
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::InvocationInsertParams;
use orbit_types::task::{TaskComplexity, TaskEventRowV2, TaskStatus};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use serde_json::{Value, json};

use super::backlog_forecast::{
    ENDPOINT, completed, completed_work, drain_hours, initial_work_forecast_baseline, prediction,
    retimestamp_invocation,
};
use super::support::{Fixture, isolated, json_ok};

fn cohort<'a>(correction: &'a Value, complexity: &str, metric: &str) -> &'a Value {
    correction["cohorts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["complexity"] == complexity && row["metric"] == metric)
        .unwrap_or_else(|| panic!("missing {complexity} {metric}: {correction}"))
}

fn stored_task(id: &str, complexity: &str, p50: f64, base: bool) -> Value {
    let quantiles = json!({"p50": p50, "p90": p50});
    let mut forecast = json!({
        "work_hours": quantiles,
        "usd": quantiles,
    });
    if base {
        forecast["base"] = json!({
            "work_hours": quantiles,
            "usd": quantiles,
        });
    }
    json!({
        "id": id,
        "complexity": complexity,
        "forecast": forecast,
    })
}

/// A stored task whose base carries spread: p90 is twice p50 for every
/// metric, so the fitted `spread` is non-zero.
fn stored_task_spread(id: &str, complexity: &str) -> Value {
    let quantiles = json!({
        "work_hours": {"p50": 1.0, "p90": 2.0},
        "tokens": {"p50": 25.0, "p90": 50.0},
        "usd": {"p50": 1.0, "p90": 2.0},
    });
    json!({
        "id": id,
        "complexity": complexity,
        "forecast": {
            "work_hours": quantiles["work_hours"],
            "tokens": quantiles["tokens"],
            "usd": quantiles["usd"],
            "base": quantiles,
        },
    })
}

/// One medium failed attempt: admitted at `at`, back in the backlog an
/// hour later, then parked in someday. Its invocation used one hour,
/// 100 input tokens and one dollar, so a drawn setback step carries all
/// three metrics.
fn failed_attempt(fixture: &Fixture, at: DateTime<Utc>) {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Failed attempt".into(),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Someday),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &fixture.runtime.global_root(),
        ),
    )
    .unwrap();
    let events_path = registry
        .find_task_binding(&id)
        .unwrap()
        .unwrap()
        .canonical_path
        .join("events.jsonl");
    let created: TaskEventRowV2 = serde_json::from_str(
        std::fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let steps = [
        (at - Duration::minutes(1), TaskStatus::Backlog),
        (at, TaskStatus::InProgress),
        (at + Duration::hours(1), TaskStatus::Backlog),
        (
            at + Duration::hours(1) + Duration::minutes(1),
            TaskStatus::Someday,
        ),
    ];
    let mut rows = Vec::new();
    let mut from = TaskStatus::Backlog;
    for (index, (when, to)) in steps.iter().enumerate() {
        rows.push(TaskEventRowV2 {
            event_id: format!("{}-{index}", created.event_id),
            at: *when,
            event_type: "status_changed".into(),
            note: None,
            from_status: Some(from),
            to_status: Some(*to),
            ..created.clone()
        });
        from = *to;
    }
    let events: String = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap() + "\n")
        .collect();
    std::fs::write(events_path, events).unwrap();
    fixture
        .runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: format!("failed-{id}"),
            account: None,
            activity_id: "implement_one".into(),
            agent: "codex".into(),
            provider: None,
            model: Some("gpt-6.1-sol".into()),
            task_ids: vec![id.clone()],
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
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE invocations SET ts = ?1 WHERE job_run_id = ?2",
                    (
                        (at + Duration::hours(1)).to_rfc3339(),
                        format!("failed-{id}"),
                    ),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

fn record(fixture: &Fixture, at: chrono::DateTime<Utc>, version: u32, tasks: Vec<Value>) {
    assert!(
        fixture
            .runtime
            .record_backlog_forecast(
                StoredBacklogForecast {
                    workspace_id: String::new(),
                    forecast_at: at,
                    model_version: version,
                    inputs_json: "{}".into(),
                    result_json: "{}".into(),
                    tasks_json: Value::Array(tasks).to_string(),
                },
                Duration::zero(),
            )
            .unwrap(),
        "forecast at {at} version {version} was refused"
    );
}

#[test]
fn biased_history_corrects_quantiles_and_does_not_train_on_itself() {
    isolated(
        "backlog_correction::biased_history_corrects_quantiles_and_does_not_train_on_itself",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let recent = now - Duration::days(1);
            for _ in 0..5 {
                completed(&fixture, recent);
            }
            let done_at = now - Duration::days(10);
            let observed = now - Duration::days(8);
            let mut medium = Vec::new();
            for _ in 0..8 {
                let id = completed_work(&fixture, done_at, TaskComplexity::Medium, 4);
                retimestamp_invocation(&fixture, &id, observed);
                medium.push(id);
            }
            let mut hard = Vec::new();
            for _ in 0..2 {
                let id = completed_work(&fixture, done_at, TaskComplexity::Hard, 4);
                retimestamp_invocation(&fixture, &id, observed);
                hard.push(id);
            }
            let pending_medium = super::backlog_forecast::backlog_task(
                &fixture,
                "Open medium",
                &["file:medium.rs"],
                &[],
                &[],
            );
            let pending_hard = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Open hard".into(),
                    complexity: TaskComplexity::Hard,
                    status: Some(TaskStatus::Backlog),
                    context_files: vec!["file:hard.rs".into()],
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();

            let mut eligible = vec![stored_task(&pending_medium, "medium", 1.0, true)];
            eligible.extend(medium.iter().map(|id| stored_task(id, "medium", 1.0, true)));
            eligible.extend(hard.iter().map(|id| stored_task(id, "hard", 1.0, true)));
            // Oldest first: a later row is not blocked by the zero interval.
            record(
                &fixture,
                now - Duration::days(13),
                14,
                medium
                    .iter()
                    .map(|id| stored_task(id, "medium", 100.0, false))
                    .collect(),
            );
            record(&fixture, now - Duration::days(12), 20, eligible);
            record(
                &fixture,
                now - Duration::days(9),
                20,
                medium
                    .iter()
                    .map(|id| stored_task(id, "medium", 100.0, true))
                    .collect(),
            );

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            assert_eq!(response["model_version"], 20);
            assert_eq!(response["inputs"]["history_deliveries"], 5, "{response}");
            assert_eq!(
                response["assumptions"]["residual_correction"],
                "per_metric_complexity_log_affine"
            );
            let correction = &response["inputs"]["correction"];
            assert_eq!(correction["half_life_days"], 14.0);
            assert_eq!(correction["window_days"], 90.0);
            assert_eq!(correction["min_n"], 8);
            let fit_ms = response["timing"]["correction_ms"].as_f64().unwrap();
            assert!(fit_ms.is_finite() && fit_ms >= 0.0, "{fit_ms}");

            let medium_hours = cohort(correction, "medium", "work_hours");
            assert_eq!(medium_hours["n"], 8, "{medium_hours}");
            assert_eq!(medium_hours["applied"], true, "{medium_hours}");
            let bias = medium_hours["bias"].as_f64().unwrap();
            assert!(
                (bias - 4.0_f64.ln()).abs() < 0.05,
                "medium bias {bias} from {medium_hours}"
            );
            assert!(medium_hours["weight"].as_f64().unwrap() > 0.0);
            let medium_usd = cohort(correction, "medium", "usd");
            assert_eq!(medium_usd["n"], 8, "{medium_usd}");
            assert_eq!(medium_usd["applied"], true, "{medium_usd}");

            let hard_hours = cohort(correction, "hard", "work_hours");
            assert_eq!(hard_hours["n"], 2, "{hard_hours}");
            assert_eq!(hard_hours["applied"], false, "{hard_hours}");

            let base_p50 = prediction(&response, &pending_medium)["base"]["work_hours"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                (0.5..1.5).contains(&base_p50),
                "stored base stays the uncorrected draw, got {base_p50}"
            );
            let medium_p50 = prediction(&response, &pending_medium)["work_hours"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                (3.5..4.5).contains(&medium_p50),
                "corrected medium p50 {medium_p50}"
            );
            let usd_p50 = prediction(&response, &pending_medium)["usd"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                (3.5..4.5).contains(&usd_p50),
                "corrected medium usd p50 {usd_p50}"
            );
            let hard_p50 = prediction(&response, &pending_hard)["work_hours"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                hard_p50 < 2.0,
                "thin hard cohort must stay near the uncorrected draw, got {hard_p50}"
            );
            let (drain_p50, _) = drain_hours(&response);
            assert!(
                drain_p50 > 4.0,
                "drain must inherit the corrected medium draw, got {drain_p50}"
            );

            let versions = response["calibration"]["by_model_version"]
                .as_array()
                .unwrap();
            let version = |number: u64| {
                versions
                    .iter()
                    .find(|row| row["model_version"] == number)
                    .unwrap_or_else(|| panic!("missing version {number}: {versions:?}"))
            };
            let older = version(14);
            assert_eq!(older["task_hours_measure"], "attempt_work_hours");
            assert!(
                older["task_level"]["hours"]["n"].as_u64().unwrap() >= 8,
                "version 14 must still be scored on its own measure: {older}"
            );
            let current = version(20);
            assert_eq!(
                current["task_level"]["hours"]["n"], 10,
                "each done task counts once, and a forecast made after done is not in the population: {current}"
            );
        },
    );
}

/// One medium task stored on ten eligible forecasts is one residual, so the
/// cohort stays under the minimum and the open draw is not shifted. Eight
/// distinct hard tasks on those same forecasts are eight residuals and are
/// corrected once each.
#[test]
fn repeated_snapshots_count_one_residual_per_task() {
    isolated(
        "backlog_correction::repeated_snapshots_count_one_residual_per_task",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            for _ in 0..5 {
                completed(&fixture, now - Duration::days(1));
            }
            let done_at = now - Duration::days(10);
            let observed = now - Duration::days(8);
            let medium_id = completed_work(&fixture, done_at, TaskComplexity::Medium, 4);
            retimestamp_invocation(&fixture, &medium_id, observed);
            let mut hard = Vec::new();
            for _ in 0..8 {
                let id = completed_work(&fixture, done_at, TaskComplexity::Hard, 4);
                retimestamp_invocation(&fixture, &id, observed);
                hard.push(id);
            }
            let pending_medium = super::backlog_forecast::backlog_task(
                &fixture,
                "Open medium",
                &["file:medium.rs"],
                &[],
                &[],
            );
            let pending_hard = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Open hard".into(),
                    complexity: TaskComplexity::Hard,
                    status: Some(TaskStatus::Backlog),
                    context_files: vec!["file:hard.rs".into()],
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();

            let mut training = vec![stored_task(&medium_id, "medium", 1.0, true)];
            training.extend(hard.iter().map(|id| stored_task(id, "hard", 1.0, true)));
            // Oldest first, every row before done, so each task is eligible
            // ten times. Ten copies of one task must not reach the minimum.
            for days_ago in (12..=21).rev() {
                record(
                    &fixture,
                    now - Duration::days(days_ago),
                    20,
                    training.clone(),
                );
            }

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            let correction = &response["inputs"]["correction"];
            let age_days = (Utc::now() - done_at).num_seconds() as f64 / 86_400.0;
            let one_weight = 0.5_f64.powf(age_days / 14.0);

            let medium_hours = cohort(correction, "medium", "work_hours");
            assert_eq!(medium_hours["n"], 1, "{medium_hours}");
            assert_eq!(medium_hours["applied"], false, "{medium_hours}");
            let medium_weight = medium_hours["weight"].as_f64().unwrap();
            assert!(
                (medium_weight - one_weight).abs() < 0.05,
                "one task must be weighted once, got {medium_weight} vs {one_weight} from {medium_hours}"
            );
            let medium_usd = cohort(correction, "medium", "usd");
            assert_eq!(medium_usd["n"], 1, "{medium_usd}");
            assert_eq!(medium_usd["applied"], false, "{medium_usd}");

            let hard_hours = cohort(correction, "hard", "work_hours");
            assert_eq!(hard_hours["n"], 8, "{hard_hours}");
            assert_eq!(hard_hours["applied"], true, "{hard_hours}");
            let hard_weight = hard_hours["weight"].as_f64().unwrap();
            assert!(
                (hard_weight - 8.0 * one_weight).abs() < 0.05,
                "eight tasks must be weighted once each, got {hard_weight} vs {} from {hard_hours}",
                8.0 * one_weight
            );
            let hard_usd = cohort(correction, "hard", "usd");
            assert_eq!(hard_usd["n"], 8, "{hard_usd}");
            assert_eq!(hard_usd["applied"], true, "{hard_usd}");

            let medium_p50 = prediction(&response, &pending_medium)["work_hours"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                (0.5..1.5).contains(&medium_p50),
                "one repeated task must leave the medium draw uncorrected, got {medium_p50}"
            );
            let hard_p50 = prediction(&response, &pending_hard)["work_hours"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                (3.5..4.5).contains(&hard_p50),
                "eight distinct tasks must correct the hard draw, got {hard_p50}"
            );
        },
    );
}

/// The correction must land on the full-script total, and it must not
/// stretch that total by its distance from the cohort center. The only
/// outcome pool is five one-hour failed attempts, so the open task's
/// script is 25 one-hour steps (the attempt cap) totalling 25 uncorrected
/// hours, 25 times the cohort center of one hour. Training took four
/// hours against a base p50 of one (p90 two), so the fitted p50 shift is
/// `ln(4)`: the 25-hour total corrects to 100 hours, a factor of four,
/// not to the `ln(10)` clamp the old cohort-center spread reached.
/// Tokens and USD follow the same total: each failed step carries them,
/// and their corrected sums must be the correction of their uncorrected
/// totals.
#[test]
fn correction_applies_p50_shift_to_the_full_script_total() {
    isolated(
        "backlog_correction::correction_applies_p50_shift_to_the_full_script_total",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            for _ in 0..5 {
                completed(&fixture, now - Duration::days(1));
            }
            let done_at = now - Duration::days(10);
            let observed = now - Duration::days(8);
            let mut medium = Vec::new();
            for _ in 0..8 {
                let id = completed_work(&fixture, done_at, TaskComplexity::Medium, 4);
                retimestamp_invocation(&fixture, &id, observed);
                medium.push(id);
            }
            for _ in 0..5 {
                failed_attempt(&fixture, now - Duration::hours(12));
            }
            let pending = super::backlog_forecast::backlog_task(
                &fixture,
                "Open multi-attempt",
                &["file:multi.rs"],
                &[],
                &[],
            );
            record(
                &fixture,
                now - Duration::days(12),
                20,
                medium
                    .iter()
                    .map(|id| stored_task_spread(id, "medium"))
                    .collect(),
            );

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            assert_eq!(
                response["inputs"]["outcomes"]["episodes"],
                json!({"failed": 5}),
                "{response}"
            );
            let correction = &response["inputs"]["correction"];
            for metric in ["work_hours", "tokens", "usd"] {
                let term = cohort(correction, "medium", metric);
                assert_eq!(term["n"], 8, "{term}");
                assert_eq!(term["applied"], true, "{term}");
                assert_eq!(term["withheld"], Value::Null, "{term}");
                let spread = term["spread"].as_f64().unwrap();
                assert!(
                    spread.abs() < 0.001,
                    "{metric} spread must not stretch a draw, got {spread} from {term}"
                );
                let center = term["center"].as_f64().unwrap();
                assert!(center > 0.0, "{metric} center: {term}");
                let holdout = &term["holdout"];
                assert_eq!(holdout["n"], 8, "{holdout}");
                assert!(
                    holdout["corrected_median_abs_log_p50_error"]
                        .as_f64()
                        .unwrap()
                        < holdout["base_median_abs_log_p50_error"].as_f64().unwrap(),
                    "{metric} holdout must expose improving p50 figures: {holdout}"
                );
            }

            let row = prediction(&response, &pending);
            let base_hours = row["base"]["work_hours"]["p50"].as_f64().unwrap();
            assert!(
                (base_hours - 25.0).abs() < 0.01,
                "uncorrected multi-step total must be 25 hours, got {base_hours}: {row}"
            );
            let hours = row["work_hours"]["p50"].as_f64().unwrap();
            assert!(
                (90.0..110.0).contains(&hours),
                "corrected steps must sum to 25 shifted by exp(ln(4)) (100), not the ln(10) clamp, got {hours}: {row}"
            );
            assert!(
                (hours / base_hours - 4.0).abs() < 0.4,
                "corrected/base ratio must be the fitted p50 factor 4, got {}: {row}",
                hours / base_hours
            );
            let usd = row["usd"]["p50"].as_f64().unwrap();
            assert!(
                (90.0..110.0).contains(&usd),
                "corrected USD must sum to the correction of its total (100), got {usd}: {row}"
            );
            let base_tokens = row["base"]["tokens"]["p50"].as_f64().unwrap();
            let tokens = row["tokens"]["p50"].as_f64().unwrap();
            let token_term = cohort(correction, "medium", "tokens");
            // The map on the total is only the fitted p50 shift.
            let shift = token_term["p50_log_adjust"]
                .as_f64()
                .unwrap()
                .clamp(-10.0_f64.ln(), 10.0_f64.ln());
            let expected_tokens = (base_tokens.ln() + shift).exp();
            assert!(
                base_tokens > 0.0 && (tokens - expected_tokens).abs() <= expected_tokens * 0.05,
                "corrected tokens must sum to the correction of their total ({expected_tokens}), got {tokens} from base {base_tokens} term {token_term}: {row}"
            );
        },
    );
}

/// A stored task whose served quantiles and base are `p50`/`p90`, taken
/// before or after its work started.
fn stored_task_state(id: &str, complexity: &str, p50: f64, p90: f64, in_progress: bool) -> Value {
    let quantiles = json!({"p50": p50, "p90": p90});
    json!({
        "id": id,
        "complexity": complexity,
        "in_progress": in_progress,
        "forecast": {
            "work_hours": quantiles,
            "usd": quantiles,
            "base": {"work_hours": quantiles, "usd": quantiles},
        },
    })
}

fn complexity_hits<'a>(calibration: &'a Value, complexity: &str) -> &'a Value {
    calibration["task_level_by_complexity"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["complexity"] == complexity)
        .unwrap_or_else(|| panic!("missing {complexity}: {calibration}"))
}

/// Each task is on three stored forecasts: two before work started and one
/// after. The fit and task-level calibration read only the first of them,
/// so the medium residual is the four hours against its first base of one
/// hour, not against the later not-started base (100) or the in-progress
/// one (0.01), and each task counts once. A low task seen only in progress
/// is in neither.
#[test]
fn fit_and_calibration_read_the_first_not_started_forecast_per_task() {
    isolated(
        "backlog_correction::fit_and_calibration_read_the_first_not_started_forecast_per_task",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            for _ in 0..5 {
                completed(&fixture, now - Duration::days(1));
            }
            let done_at = now - Duration::days(10);
            let observed = now - Duration::days(8);
            let done = |complexity: TaskComplexity, count: usize| -> Vec<String> {
                (0..count)
                    .map(|_| {
                        let id = completed_work(&fixture, done_at, complexity, 4);
                        retimestamp_invocation(&fixture, &id, observed);
                        id
                    })
                    .collect()
            };
            let medium = done(TaskComplexity::Medium, 8);
            let low = done(TaskComplexity::Low, 2);
            let hard = done(TaskComplexity::Hard, 2);
            let started_low = done(TaskComplexity::Low, 1);
            let pending = super::backlog_forecast::backlog_task(
                &fixture,
                "Open medium",
                &["file:medium.rs"],
                &[],
                &[],
            );

            let row =
                |p50: f64, in_progress: bool| -> Vec<Value> {
                    let mut tasks = vec![stored_task_state(&pending, "medium", 1.0, 1.0, false)];
                    tasks.extend(
                        medium
                            .iter()
                            .map(|id| stored_task_state(id, "medium", p50, p50, in_progress)),
                    );
                    // Low is served wide enough to hit, hard is not.
                    tasks.extend(low.iter().map(|id| {
                        stored_task_state(id, "low", 10.0 * p50, 10.0 * p50, in_progress)
                    }));
                    tasks.extend(
                        hard.iter()
                            .map(|id| stored_task_state(id, "hard", p50, p50, in_progress)),
                    );
                    tasks.extend(
                        started_low
                            .iter()
                            .map(|id| stored_task_state(id, "low", 10.0, 10.0, true)),
                    );
                    tasks
                };
            // Oldest first: the first not-started forecast, a later
            // not-started one, then one taken after work started.
            record(&fixture, now - Duration::days(14), 20, row(1.0, false));
            record(&fixture, now - Duration::days(13), 20, row(100.0, false));
            record(&fixture, now - Duration::days(12), 20, row(0.01, true));

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            let correction = &response["inputs"]["correction"];
            assert_eq!(
                correction["population"], "first_not_started_forecast_per_task",
                "{correction}"
            );
            let medium_hours = cohort(correction, "medium", "work_hours");
            assert_eq!(medium_hours["n"], 8, "{medium_hours}");
            assert_eq!(medium_hours["applied"], true, "{medium_hours}");
            let bias = medium_hours["bias"].as_f64().unwrap();
            assert!(
                (bias - 4.0_f64.ln()).abs() < 0.05,
                "the first not-started base (1h) is the residual, got bias {bias}: {medium_hours}"
            );
            let low_hours = cohort(correction, "low", "work_hours");
            assert_eq!(
                low_hours["n"], 2,
                "a task seen only in progress is not a residual: {low_hours}"
            );

            let calibration = &response["calibration"];
            assert_eq!(
                calibration["task_population"], "first_not_started_forecast_per_task",
                "{calibration}"
            );
            let hours = &calibration["task_level"]["hours"];
            assert_eq!(hours["n"], 12, "each task counts once: {hours}");
            assert_eq!(hours["under_p50"], 2.0 / 12.0, "{hours}");
            for (complexity, n, under) in [("low", 2, 1.0), ("medium", 8, 0.0), ("hard", 2, 0.0)] {
                let row = complexity_hits(calibration, complexity);
                for metric in ["hours", "usd"] {
                    assert_eq!(row[metric]["n"], n, "{complexity} {metric}: {row}");
                    assert_eq!(
                        row[metric]["under_p50"], under,
                        "{complexity} {metric}: {row}"
                    );
                    assert_eq!(
                        row[metric]["under_p90"], under,
                        "{complexity} {metric}: {row}"
                    );
                }
                assert_eq!(row["tokens"]["n"], 0, "no stored token quantiles: {row}");
            }
            let current = calibration["by_model_version"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["model_version"] == 20)
                .unwrap();
            assert_eq!(
                current["task_population"],
                "first_not_started_forecast_per_task"
            );
            assert_eq!(current["task_level"]["hours"]["n"], 12, "{current}");
        },
    );
}

/// Twelve medium tasks against a base p50 of two hours (p90 20): nine
/// took one hour and three took 10, 14 and 18. The fitted p50 shift
/// halves every draw, so held out the 14- and 18-hour tasks miss the
/// corrected p90 of ten the other eleven fit, and leave-one-out coverage
/// is 10/12 against a base of 12/12. The cohort meets the minimum and is
/// withheld, and the open draw stays at the base.
#[test]
fn cohort_withheld_when_holdout_p90_falls_below_base() {
    isolated(
        "backlog_correction::cohort_withheld_when_holdout_p90_falls_below_base",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            for _ in 0..5 {
                completed(&fixture, now - Duration::days(1));
            }
            let done_at = now - Duration::days(10);
            let observed = now - Duration::days(8);
            let mut medium = Vec::new();
            for hours in [1, 1, 1, 1, 1, 1, 1, 1, 1, 10, 14, 18] {
                let id = completed_work(&fixture, done_at, TaskComplexity::Medium, hours);
                retimestamp_invocation(&fixture, &id, observed);
                medium.push(id);
            }
            let pending = super::backlog_forecast::backlog_task(
                &fixture,
                "Open medium",
                &["file:medium.rs"],
                &[],
                &[],
            );
            record(
                &fixture,
                now - Duration::days(12),
                20,
                medium
                    .iter()
                    .map(|id| stored_task_state(id, "medium", 2.0, 20.0, false))
                    .collect(),
            );

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            let correction = &response["inputs"]["correction"];
            for metric in ["work_hours", "usd"] {
                let term = cohort(correction, "medium", metric);
                assert_eq!(term["n"], 12, "{term}");
                assert_eq!(term["applied"], false, "{metric}: {term}");
                assert_eq!(term["withheld"], "p90_holdout", "{metric}: {term}");
                let holdout = &term["holdout"];
                assert_eq!(holdout["method"], "leave_one_out", "{holdout}");
                assert_eq!(holdout["n"], 12, "{holdout}");
                assert_eq!(holdout["base_under_p90"], 1.0, "{holdout}");
                assert_eq!(holdout["corrected_under_p90"], 0.833, "{holdout}");
            }
            let p50 = prediction(&response, &pending)["work_hours"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                (0.5..1.5).contains(&p50),
                "a withheld cohort leaves the draw uncorrected, got {p50}"
            );
        },
    );
}

/// A stored task reproducing the audit's token scale in ratio terms:
/// base tokens p50 is `100 / exp(0.828)`, so every token residual is
/// 0.828 and the cohort center is that base, far below an open task whose
/// 25 failed steps total 2,500 base tokens.
fn stored_task_audit(id: &str, complexity: &str) -> Value {
    json!({
        "id": id,
        "complexity": complexity,
        "forecast": {
            "work_hours": {"p50": 1.0, "p90": 2.0},
            "tokens": {"p50": 43.719, "p90": 87.438},
            "usd": {"p50": 1.0, "p90": 2.0},
            "base": {
                "work_hours": {"p50": 1.0, "p90": 2.0},
                "tokens": {"p50": 43.719, "p90": 87.438},
                "usd": {"p50": 1.0, "p90": 2.0},
            },
        },
    })
}

#[test]
fn audit_scale_task_tokens_p50_is_not_clamped_to_ten_times_base() {
    isolated(
        "backlog_correction::audit_scale_task_tokens_p50_is_not_clamped_to_ten_times_base",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            for _ in 0..5 {
                completed(&fixture, now - Duration::days(1));
            }
            let done_at = now - Duration::days(10);
            let observed = now - Duration::days(8);
            let mut medium = Vec::new();
            for _ in 0..8 {
                let id = completed_work(&fixture, done_at, TaskComplexity::Medium, 4);
                retimestamp_invocation(&fixture, &id, observed);
                medium.push(id);
            }
            for _ in 0..5 {
                failed_attempt(&fixture, now - Duration::hours(12));
            }
            let pending = super::backlog_forecast::backlog_task(
                &fixture,
                "Open audit scale",
                &["file:audit.rs"],
                &[],
                &[],
            );
            record(
                &fixture,
                now - Duration::days(12),
                20,
                medium
                    .iter()
                    .map(|id| stored_task_audit(id, "medium"))
                    .collect(),
            );

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            let correction = &response["inputs"]["correction"];
            let term = cohort(correction, "medium", "tokens");
            assert_eq!(term["n"], 8, "{term}");
            assert_eq!(term["applied"], true, "{term}");
            let q50 = term["p50_log_adjust"].as_f64().unwrap();
            assert!((q50 - 0.828).abs() < 0.05, "q50 {q50}: {term}");
            assert_eq!(term["spread"], 0.0, "{term}");
            let center = term["center"].as_f64().unwrap();
            assert!((center - 43.719).abs() < 0.1, "center {center}: {term}");

            let row = prediction(&response, &pending);
            let base = row["base"]["tokens"]["p50"].as_f64().unwrap();
            let corrected = row["tokens"]["p50"].as_f64().unwrap();
            assert!(
                base > 4.0 * center,
                "fixture base {base} must sit far from cohort center {center}: {row}"
            );
            let ratio = corrected / base;
            assert!(
                (2.0..2.6).contains(&ratio),
                "corrected/base must be exp(q50) (~2.29), not the 10x clamp, got {ratio} from base {base} corrected {corrected}: {row}"
            );
            assert!(corrected < base * 9.9, "must not reach clamp: {row}");
        },
    );
}

/// Twelve medium tasks against a wide base (p50 one hour, p90 100):
/// six took one hour and six took four. Held out, a one-hour task is
/// corrected by a cohort whose median residual is `ln(4)` and a four-hour
/// task by one whose median is zero, so every held-out corrected p50 is
/// `ln(4)` from its actual while the base median absolute log error is
/// half that. P90 coverage stays perfect on both sides, so only the new
/// p50 holdout can withhold the cohort.
#[test]
fn cohort_withheld_when_holdout_p50_error_worsens() {
    isolated(
        "backlog_correction::cohort_withheld_when_holdout_p50_error_worsens",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            for _ in 0..5 {
                completed(&fixture, now - Duration::days(1));
            }
            let done_at = now - Duration::days(10);
            let observed = now - Duration::days(8);
            let mut medium = Vec::new();
            for hours in [1, 1, 1, 1, 1, 1, 4, 4, 4, 4, 4, 4] {
                let id = completed_work(&fixture, done_at, TaskComplexity::Medium, hours);
                retimestamp_invocation(&fixture, &id, observed);
                medium.push(id);
            }
            let pending = super::backlog_forecast::backlog_task(
                &fixture,
                "Open medium",
                &["file:medium.rs"],
                &[],
                &[],
            );
            record(
                &fixture,
                now - Duration::days(12),
                20,
                medium
                    .iter()
                    .map(|id| stored_task_state(id, "medium", 1.0, 100.0, false))
                    .collect(),
            );

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            let correction = &response["inputs"]["correction"];
            for metric in ["work_hours", "usd"] {
                let term = cohort(correction, "medium", metric);
                assert_eq!(term["n"], 12, "{term}");
                assert_eq!(term["applied"], false, "{metric}: {term}");
                assert_eq!(term["withheld"], "p50_holdout", "{metric}: {term}");
                let holdout = &term["holdout"];
                assert_eq!(holdout["method"], "leave_one_out", "{holdout}");
                assert_eq!(holdout["n"], 12, "{holdout}");
                assert_eq!(holdout["base_under_p90"], 1.0, "{holdout}");
                assert_eq!(holdout["corrected_under_p90"], 1.0, "{holdout}");
                let base_error = holdout["base_median_abs_log_p50_error"].as_f64().unwrap();
                let corrected_error = holdout["corrected_median_abs_log_p50_error"]
                    .as_f64()
                    .unwrap();
                assert!(
                    (base_error - 0.693).abs() < 0.01,
                    "{metric} base p50 error {base_error}: {holdout}"
                );
                assert!(
                    (corrected_error - 1.386).abs() < 0.01,
                    "{metric} corrected p50 error {corrected_error}: {holdout}"
                );
                assert_eq!(
                    holdout["base_p50_median_abs_log_error"],
                    holdout["base_median_abs_log_p50_error"],
                    "{holdout}"
                );
            }
            let p50 = prediction(&response, &pending)["work_hours"]["p50"]
                .as_f64()
                .unwrap();
            assert!(
                (0.5..1.5).contains(&p50),
                "a p50-withheld cohort leaves the draw uncorrected, got {p50}"
            );
        },
    );
}
