//! Residual correction at the public forecast API.
//!
//! Delivery history is five one-hour medium tasks, so an uncorrected draw is
//! one hour. Eight older medium tasks actually took four hours against a
//! stored base of one hour. The open tasks are not that history: the newest
//! five completions are the one-hour draws.

use chrono::{Duration, Utc};
use orbit_core::StoredBacklogForecast;
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{TaskComplexity, TaskStatus};
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
            record(&fixture, now - Duration::days(12), 15, eligible);
            record(
                &fixture,
                now - Duration::days(9),
                15,
                medium
                    .iter()
                    .map(|id| stored_task(id, "medium", 100.0, true))
                    .collect(),
            );

            let server = fixture.server(false);
            let response = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&history_tasks=5&concurrency=1")),
            ));
            assert_eq!(response["model_version"], 15);
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
            let current = version(15);
            assert!(
                current["task_level"]["hours"]["n"].as_u64().unwrap() > 8,
                "scoring still counts a forecast made after done: {current}"
            );
        },
    );
}
