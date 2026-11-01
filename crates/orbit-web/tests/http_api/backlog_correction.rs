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
            record(&fixture, now - Duration::days(12), 17, eligible);
            record(
                &fixture,
                now - Duration::days(9),
                17,
                medium
                    .iter()
                    .map(|id| stored_task(id, "medium", 100.0, true))
                    .collect(),
            );

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            assert_eq!(response["model_version"], 17);
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
            let current = version(17);
            assert!(
                current["task_level"]["hours"]["n"].as_u64().unwrap() > 8,
                "scoring still counts a forecast made after done: {current}"
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
                    17,
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

/// With stored base p90 > p50 the fitted spread is non-zero, and the
/// correction must land on the full-script total. The only outcome pool
/// is five one-hour failed attempts, so the open task's script is 25
/// one-hour steps (the attempt cap) totalling 25 uncorrected hours.
/// Training took four hours against a base p50 of one (p90 two), which
/// maps a 25-hour total to four hours; applying the map per step would
/// instead give 25 corrected steps. Tokens and USD follow the same
/// total: each failed step carries them, and their corrected sums must
/// be the correction of their uncorrected totals, not 25 corrections.
#[test]
fn spread_correction_applies_to_the_full_script_total() {
    isolated(
        "backlog_correction::spread_correction_applies_to_the_full_script_total",
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
                17,
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
                let spread = term["spread"].as_f64().unwrap();
                assert!(
                    (spread + 1.0).abs() < 0.05,
                    "{metric} spread must be non-zero, got {spread} from {term}"
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
                (3.5..4.5).contains(&hours),
                "corrected steps must sum to the correction of the 25-hour total (4), got {hours}: {row}"
            );
            let usd = row["usd"]["p50"].as_f64().unwrap();
            assert!(
                (3.5..4.5).contains(&usd),
                "corrected USD must sum to the correction of its total (4), got {usd}: {row}"
            );
            let base_tokens = row["base"]["tokens"]["p50"].as_f64().unwrap();
            let tokens = row["tokens"]["p50"].as_f64().unwrap();
            let token_term = cohort(correction, "medium", "tokens");
            // The log-affine map on the total, with the same ±ln(10)
            // shift clamp the fit applies to a draw.
            let shift = (token_term["p50_log_adjust"].as_f64().unwrap()
                + token_term["spread"].as_f64().unwrap()
                    * (base_tokens.ln() - token_term["center"].as_f64().unwrap().ln()))
            .clamp(-10.0_f64.ln(), 10.0_f64.ln());
            let expected_tokens = (base_tokens.ln() + shift).exp();
            assert!(
                base_tokens > 0.0 && (tokens - expected_tokens).abs() <= expected_tokens * 0.05,
                "corrected tokens must sum to the correction of their total ({expected_tokens}), got {tokens} from base {base_tokens} term {token_term}: {row}"
            );
        },
    );
}
