//! Scheduled snapshots, completion dates and incomplete arrival telemetry at
//! the public clock and HTTP forecast boundaries.

use chrono::{DateTime, Duration, Utc};
use orbit_core::StoredBacklogForecast;
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::{InvocationInsertParams, TaskHistoryUpdateParams};
use orbit_types::task::{TaskComment, TaskComplexity, TaskEnvelopeV2, TaskEventRowV2, TaskStatus};
use orbit_types::telemetry::{InvocationTrace, TokenUsage};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

pub(super) const ENDPOINT: &str = "/api/diagnostics/backlog_forecast?workspace=ws_http_fixture";

struct ForecastWorkspace<'a>(&'a Fixture);

impl orbit_core::application::routines::loader::RoutineWorkspaceProvider for ForecastWorkspace<'_> {
    fn discover_workspaces(
        &self,
        _: &std::path::Path,
    ) -> Result<
        orbit_core::application::routines::loader::DiscoveredWorkspaces,
        orbit_core::OrbitError,
    > {
        let registry = orbit_registry::workspace_registry::load_registry_from(
            &orbit_registry::workspace_registry::registry_path_for(&self.0.global),
        )?;
        // Deliberately include even a replica in entries: the write boundary
        // must defend ownership as well as production discovery's partition.
        Ok(
            orbit_core::application::routines::loader::DiscoveredWorkspaces {
                entries: vec![(registry.workspaces[0].clone(), self.0.runtime.clone())],
                ..Default::default()
            },
        )
    }
}

pub(super) fn forecast_tick(fixture: &Fixture, now: DateTime<Utc>, dry_run: bool) {
    let outcome = orbit_core::test_support::run_sweep_at(
        &fixture.global,
        orbit_core::application::routines::SweepOptions {
            dry_run,
            ..Default::default()
        },
        orbit_core::application::routines::RoutineMachineIdentity {
            machine_id: "forecast-fixture".into(),
            machine_name: "forecast-fixture".into(),
        },
        &ForecastWorkspace(fixture),
        now,
    )
    .expect("forecast failures never fail the clock tick");
    assert!(!outcome.lock_busy);
    assert!(!outcome.deadline_exceeded);
}

#[test]
fn clock_records_canonical_forecasts_without_endpoint_reads() {
    isolated(
        "backlog_forecast::clock_records_canonical_forecasts_without_endpoint_reads",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            completed(&fixture, now - Duration::days(1));
            forecast_tick(&fixture, now, false);
            assert!(
                fixture.runtime.backlog_forecasts(10).unwrap().is_empty(),
                "empty backlog"
            );

            fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Unattended backlog".into(),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            forecast_tick(&fixture, now, true);
            assert!(
                fixture.runtime.backlog_forecasts(10).unwrap().is_empty(),
                "dry run"
            );
            forecast_tick(&fixture, now, false);
            let rows = fixture.runtime.backlog_forecasts(10).unwrap();
            assert_eq!(
                rows.len(),
                1,
                "the owner clock must store a forecast without a GET"
            );
            assert_eq!(
                rows[0].forecast_at.timestamp_micros(),
                now.timestamp_micros()
            );
            let inputs: Value = serde_json::from_str(&rows[0].inputs_json).unwrap();
            assert_eq!(inputs["lookback_days"], 30);
            assert_eq!(inputs["arrivals"], false);

            // The default read serves the clock's snapshot, recording nothing.
            let server = fixture.server(false);
            let read = json_ok(server.get(ENDPOINT));
            assert_eq!(read["served_from"], "stored");
            assert_eq!(read["stored"]["recorded"], Value::Null);
            assert_eq!(
                DateTime::parse_from_rfc3339(read["generated_at"].as_str().unwrap())
                    .unwrap()
                    .timestamp_micros(),
                now.timestamp_micros()
            );
            let mut read_inputs = read["inputs"].clone();
            let mut stored_inputs = inputs;
            read_inputs
                .as_object_mut()
                .unwrap()
                .remove("lookback_start");
            stored_inputs
                .as_object_mut()
                .unwrap()
                .remove("lookback_start");
            assert_eq!(read_inputs, stored_inputs);
            // Scoring reads the quantiles beside the served snapshot; per-task
            // predictions are scored from each stored task.
            let mut read_forecast = read["forecast"].clone();
            let predictions = read_forecast
                .as_object_mut()
                .unwrap()
                .remove("task_predictions")
                .unwrap();
            let mut stored_result: Value = serde_json::from_str(&rows[0].result_json).unwrap();
            let snapshot = stored_result
                .as_object_mut()
                .unwrap()
                .remove("snapshot")
                .unwrap();
            assert_eq!(read_forecast, stored_result);
            assert_eq!(snapshot["calibration"], read["calibration"]);
            assert_eq!(snapshot["coverage"], read["coverage"]);
            let stored_tasks: Value = serde_json::from_str(&rows[0].tasks_json).unwrap();
            assert_eq!(
                stored_tasks[0]["predicted_start_hours"],
                predictions[0]["predicted_start_hours"]
            );
            forecast_tick(&fixture, now + Duration::minutes(59), false);
            assert_eq!(fixture.runtime.backlog_forecasts(10).unwrap().len(), 1);
            forecast_tick(&fixture, now + Duration::minutes(60), false);
            let rows = fixture.runtime.backlog_forecasts(10).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(
                rows[0].forecast_at.timestamp_micros(),
                (now + Duration::minutes(60)).timestamp_micros()
            );
        },
    );
}

#[test]
fn clock_forecast_skips_replica_and_missing_history_and_contains_write_failure() {
    isolated(
        "backlog_forecast::clock_forecast_skips_replica_and_missing_history_and_contains_write_failure",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Unattended backlog".into(),
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            forecast_tick(&fixture, now, false);
            assert!(
                fixture.runtime.backlog_forecasts(10).unwrap().is_empty(),
                "no delivery history"
            );
            completed(&fixture, now - Duration::days(1));

            let log_path = fixture.path("forecast-warnings.log");
            let subscriber = tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(
                    std::fs::File::create(&log_path).unwrap(),
                ))
                .finish();
            let _subscriber = tracing::subscriber::set_default(subscriber);
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    tx.connection()
                        .execute_batch(
                            "CREATE TRIGGER refuse_forecast BEFORE INSERT ON backlog_forecasts
                             BEGIN SELECT RAISE(ABORT, 'fixture forecast write refused'); END;",
                        )
                        .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                    Ok(())
                })
                .unwrap();
            forecast_tick(&fixture, now, false);
            assert!(fixture.runtime.backlog_forecasts(10).unwrap().is_empty());
            let log = std::fs::read_to_string(&log_path).unwrap();
            assert!(
                log.contains("fixture forecast write refused"),
                "write failure must be logged: {log}"
            );
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    tx.connection()
                        .execute_batch("DROP TRIGGER refuse_forecast;")
                        .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                    Ok(())
                })
                .unwrap();

            // A read winning the slot and then the clock still leave one row.
            let server = fixture.server(false);
            assert_eq!(json_ok(server.get(ENDPOINT))["stored"]["recorded"], true);
            forecast_tick(&fixture, Utc::now(), false);
            assert_eq!(fixture.runtime.backlog_forecasts(10).unwrap().len(), 1);
            drop(server);
            let fixture = fixture.into_replica("hm_forecast_owner");
            forecast_tick(&fixture, now + Duration::hours(2), false);
            assert_eq!(
                fixture.runtime.backlog_forecasts(10).unwrap().len(),
                1,
                "replica cannot fill a free slot"
            );
        },
    );
}

#[test]
fn clock_records_blocked_only_cohort_without_endpoint_reads() {
    isolated(
        "backlog_forecast::clock_records_blocked_only_cohort_without_endpoint_reads",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            one_hour_history(&fixture);
            forecast_tick(&fixture, now, false);
            assert!(
                fixture.runtime.backlog_forecasts(10).unwrap().is_empty(),
                "no forecastable work yet"
            );

            fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Blocked only".into(),
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                })
                .unwrap();
            forecast_tick(&fixture, now, true);
            assert!(
                fixture.runtime.backlog_forecasts(10).unwrap().is_empty(),
                "dry run"
            );
            forecast_tick(&fixture, now, false);
            let rows = fixture.runtime.backlog_forecasts(10).unwrap();
            assert_eq!(
                rows.len(),
                1,
                "the owner clock must store a blocked-only forecast without a GET"
            );
            let stored_tasks: Value = serde_json::from_str(&rows[0].tasks_json).unwrap();
            assert_eq!(stored_tasks.as_array().unwrap().len(), 1);
            assert_eq!(stored_tasks[0]["status"], "blocked");

            let server = fixture.server(false);
            let read = json_ok(server.get(ENDPOINT));
            assert_eq!(read["served_from"], "stored");
            assert_eq!(read["backlog"]["blocked"], 1);
            drop(server);

            forecast_tick(&fixture, now + Duration::minutes(59), false);
            assert_eq!(fixture.runtime.backlog_forecasts(10).unwrap().len(), 1);
            forecast_tick(&fixture, now + Duration::minutes(60), false);
            assert_eq!(fixture.runtime.backlog_forecasts(10).unwrap().len(), 2);

            let fixture = fixture.into_replica("hm_forecast_owner");
            forecast_tick(&fixture, now + Duration::hours(2), false);
            assert_eq!(
                fixture.runtime.backlog_forecasts(10).unwrap().len(),
                2,
                "replica cannot fill a free slot"
            );
        },
    );
}

fn forecastable(fixture: &Fixture) {
    completed(fixture, Utc::now() - Duration::days(1));
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Forecast backlog".into(),
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
}

#[test]
fn default_read_serves_the_fresh_snapshot_and_other_reads_compute_live() {
    isolated(
        "backlog_forecast::default_read_serves_the_fresh_snapshot_and_other_reads_compute_live",
        || {
            let fixture = Fixture::new();
            forecastable(&fixture);
            let server = fixture.server(false);

            // No stored row: computed live, and the free slot is filled.
            let first = json_ok(server.get(ENDPOINT));
            assert_eq!(first["served_from"], "live");
            assert_eq!(first["stored"]["recorded"], true);
            let rows = fixture.runtime.backlog_forecasts(10).unwrap();
            assert_eq!(rows.len(), 1);

            let served = json_ok(server.get(ENDPOINT));
            assert_eq!(served["served_from"], "stored");
            assert_eq!(served["generated_at"], first["generated_at"]);
            assert_eq!(served["timing"], first["timing"], "not recomputed");
            assert_eq!(served["calibration"], first["calibration"]);

            for query in [
                "&lookback_days=60",
                "&history_tasks=10",
                "&concurrency=2",
                "&arrivals=on",
                "&refresh=1",
            ] {
                let read = json_ok(server.get(&format!("{ENDPOINT}{query}")));
                assert_eq!(read["served_from"], "live", "{query}");
                assert_ne!(read["generated_at"], first["generated_at"], "{query}");
            }
            // Refresh keeps the hourly throttle and never replaces the row.
            let refreshed = json_ok(server.get(&format!("{ENDPOINT}&refresh=1")));
            assert_eq!(refreshed["stored"]["recorded"], false);
            assert_eq!(fixture.runtime.backlog_forecasts(10).unwrap(), rows);
            assert_eq!(
                server.get(&format!("{ENDPOINT}&refresh=yes")).status(),
                axum::http::StatusCode::BAD_REQUEST
            );
        },
    );
}

#[test]
fn stale_and_legacy_rows_compute_live_and_stay_readable() {
    isolated(
        "backlog_forecast::stale_and_legacy_rows_compute_live_and_stay_readable",
        || {
            // Within two store intervals the clock's snapshot is served.
            let fixture = Fixture::new();
            forecastable(&fixture);
            forecast_tick(&fixture, Utc::now() - Duration::minutes(110), false);
            let server = fixture.server(false);
            assert_eq!(json_ok(server.get(ENDPOINT))["served_from"], "stored");
            drop(server);

            // Older than that: computed live, which fills the free slot.
            let fixture = Fixture::new();
            forecastable(&fixture);
            forecast_tick(&fixture, Utc::now() - Duration::minutes(121), false);
            let server = fixture.server(false);
            let stale = json_ok(server.get(ENDPOINT));
            assert_eq!(stale["served_from"], "live");
            assert_eq!(stale["stored"]["recorded"], true);
            assert_eq!(json_ok(server.get(ENDPOINT))["served_from"], "stored");
            drop(server);

            // A row as stored before the snapshot field is scored, never served.
            let fixture = Fixture::new();
            forecastable(&fixture);
            forecast_tick(&fixture, Utc::now() - Duration::minutes(5), false);
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    tx.connection()
                        .execute_batch(
                            "UPDATE backlog_forecasts SET result_json = json_remove(result_json, '$.snapshot');",
                        )
                        .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                    Ok(())
                })
                .unwrap();
            let server = fixture.server(false);
            let legacy = json_ok(server.get(ENDPOINT));
            assert_eq!(legacy["served_from"], "live");
            assert_eq!(legacy["stored"]["recorded"], false);
            assert_eq!(
                legacy["calibration"]["forecasts"].as_array().unwrap().len(),
                1
            );
            assert_eq!(fixture.runtime.backlog_forecasts(10).unwrap().len(), 1);
        },
    );
}

pub(super) fn history(
    fixture: &Fixture,
) -> std::sync::Arc<dyn orbit_store::contracts::TaskHistoryStoreBackend> {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &fixture.runtime.global_root(),
        ),
    )
    .unwrap();
    orbit_store::compose::workspace_coordinated_backends(
        registry,
        fixture.runtime.workspace_id().unwrap(),
        fixture.runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task
    .history
}

/// Import a historical completion and its one-hour delivery, then date its
/// index projection to match. Later writes use the real bundle writer.
pub(super) fn completed(fixture: &Fixture, at: DateTime<Utc>) -> String {
    completed_delivery(
        fixture,
        at,
        TaskComplexity::Medium,
        "gpt-6.1-sol",
        Some(1.0),
        1,
    )
}

fn completed_delivery(
    fixture: &Fixture,
    at: DateTime<Utc>,
    complexity: TaskComplexity,
    model: &str,
    usd: Option<f64>,
    hours: u64,
) -> String {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Historical delivery".into(),
            complexity,
            status: Some(TaskStatus::Done),
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
    let bundle = registry
        .find_task_binding(&id)
        .unwrap()
        .unwrap()
        .canonical_path;
    // Rewind the canonical records too: opening the HTTP server refreshes
    // the index from task.yaml, so dating only the projection is insufficient.
    let envelope_path = bundle.join("task.yaml");
    let mut envelope: TaskEnvelopeV2 =
        serde_yaml::from_str(&std::fs::read_to_string(&envelope_path).unwrap()).unwrap();
    envelope.created_at = at - Duration::hours(1);
    envelope.updated_at = at;
    std::fs::write(&envelope_path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
    let events_path = bundle.join("events.jsonl");
    let events = std::fs::read_to_string(&events_path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut event: TaskEventRowV2 = serde_json::from_str(line).unwrap();
            event.at = at;
            serde_json::to_string(&event).unwrap() + "\n"
        })
        .collect::<String>();
    std::fs::write(events_path, events).unwrap();
    fixture
        .runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: format!("delivery-{id}"),
            account: None,
            activity_id: "implement_one".into(),
            agent: "codex".into(),
            provider: None,
            model: Some(model.into()),
            task_ids: vec![id.clone()],
            trace: InvocationTrace {
                duration_ms: hours * 3_600_000,
                usage: TokenUsage {
                    input: 100,
                    ..Default::default()
                },
                provider_cost_usd: usd,
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
                    (at.to_rfc3339(), format!("delivery-{id}")),
                )
                .map_err(|e| orbit_core::OrbitError::Store(e.to_string()))?;
            Ok(())
        })
        .unwrap();
    let index = rusqlite::Connection::open(fixture.global.join("tasks/index.sqlite")).unwrap();
    assert_eq!(index.execute(
        "UPDATE task_bundle_index SET created_at = ?1, updated_at = ?2, terminal_month = ?3 WHERE task_id = ?4",
        (
            (at - Duration::hours(1)).to_rfc3339(),
            at.to_rfc3339(),
            at.format("%Y-%m").to_string(),
            &id,
        ),
    ).unwrap(), 1);
    id
}

pub(super) fn comment_on_task(fixture: &Fixture, id: &str) {
    history(fixture)
        .update_task_history(
            id,
            TaskHistoryUpdateParams {
                actor: "fixture".into(),
                append_comments: vec![TaskComment {
                    at: Utc::now(),
                    by: "fixture".into(),
                    message: "Delivery evidence recorded after completion".into(),
                    operation_id: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
}

fn remove_history(fixture: &Fixture, id: &str) {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &fixture.runtime.global_root(),
        ),
    )
    .unwrap();
    let bundle = registry
        .find_task_binding(id)
        .unwrap()
        .unwrap()
        .canonical_path;
    std::fs::remove_file(bundle.join("events.jsonl")).unwrap();
}

#[test]
fn terminal_comments_preserve_completion_history_and_calibration() {
    isolated(
        "backlog_forecast::terminal_comments_preserve_completion_history_and_calibration",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let done_at = now - Duration::days(2);
            let recent = completed(&fixture, done_at);
            let old = completed(&fixture, now - Duration::days(40));
            let quantiles = json!({"p50": 1.0, "p90": 2.0});
            assert!(
                fixture
                    .runtime
                    .record_backlog_forecast(
                        StoredBacklogForecast {
                            workspace_id: String::new(),
                            forecast_at: done_at - Duration::hours(1),
                            model_version: 1,
                            inputs_json: "{}".into(),
                            result_json: json!({"time_to_drain_hours": quantiles}).to_string(),
                            tasks_json: json!([{
                                "id": recent, "complexity": "medium",
                                "forecast": {"hours": quantiles},
                            }])
                            .to_string(),
                        },
                        Duration::minutes(60)
                    )
                    .unwrap()
            );

            let server = fixture.server(false);
            let before = json_ok(server.get(ENDPOINT));
            // Fewer than 50 completions in 30 days: the floor reaches back to
            // the 40-day-old one, and the lookback starts at its completion.
            assert_eq!(before["inputs"]["history_deliveries"], 2);
            let lookback_start: DateTime<Utc> = before["inputs"]["lookback_start"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!(
                (now - lookback_start - Duration::days(40))
                    .num_minutes()
                    .abs()
                    < 60
            );
            assert_eq!(before["inputs"]["history_without_invocations"], 0);
            assert_eq!(before["arrivals"]["done_per_day"], 0.14);
            let calibration = &before["calibration"];
            assert_eq!(
                calibration["forecasts"][0]["actual"]["time_to_drain_hours"],
                1.0
            );
            assert_eq!(calibration["forecasts"][0]["scored_whole"], true);
            // Drain actual 1.0h against a 1.0h p50: a zero log residual.
            assert_eq!(
                calibration["forecasts"][0]["residual"]["time_to_drain_hours"],
                0.0
            );
            // A model-1 forecast is scored under its own version, never
            // blended into the current model's hit rates.
            assert_eq!(calibration["model_version"], before["model_version"]);
            assert_eq!(calibration["forecast_level"]["time_to_drain_hours"]["n"], 0);
            assert_eq!(calibration["by_model_version"][0]["model_version"], 1);
            assert_eq!(
                calibration["by_model_version"][0]["forecast_level"]["time_to_drain_hours"],
                json!({"n": 1, "under_p50": 1.0, "under_p90": 1.0})
            );

            for id in [&recent, &old] {
                comment_on_task(&fixture, id);
                // The old helper keeps its documented updated_at semantics.
                let rows = fixture
                    .runtime
                    .task_terminal_transitions(Some(now))
                    .unwrap();
                assert!(rows.iter().any(|row| row.task_id == *id && row.at >= now));
                let after = json_ok(server.get(ENDPOINT));
                assert_eq!(
                    after["calibration"], *calibration,
                    "terminal metadata must not move a settled forecast's actuals"
                );
                assert_eq!(
                    after["inputs"]["lookback_start"], before["inputs"]["lookback_start"],
                    "touching an old delivery must not move its completion"
                );
                assert_eq!(after["inputs"]["history_deliveries"], 2);
                assert_eq!(
                    after["arrivals"]["done_per_day"], 0.14,
                    "terminal metadata is not a new completion"
                );
            }
            // At the maximum lookback both deliveries count, while only the
            // recent completion contributes to the seven-day done rate.
            let wide: Value = json_ok(server.get(&format!("{ENDPOINT}&lookback_days=180")));
            assert_eq!(wide["inputs"]["history_deliveries"], 2);
            assert_eq!(wide["arrivals"]["done_per_day"], 0.14);
        },
    );
}

#[test]
fn unreadable_done_history_does_not_fail_the_forecast_or_count_as_done() {
    isolated(
        "backlog_forecast::unreadable_done_history_does_not_fail_the_forecast_or_count_as_done",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let done_at = now - Duration::days(2);
            let unreadable = completed(&fixture, done_at);
            completed(&fixture, done_at);
            remove_history(&fixture, &unreadable);

            let server = fixture.server(false);
            let response = server.get(ENDPOINT);
            let status = response.status();
            let body = response.text().unwrap();
            assert_eq!(status, reqwest::StatusCode::OK, "{body}");
            let forecast: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(forecast["inputs"]["history_deliveries"], 1);
            assert_eq!(forecast["inputs"]["history_without_invocations"], 0);
            assert_eq!(forecast["arrivals"]["done_per_day"], 0.14);
        },
    );
}

#[test]
fn arrivals_preserve_missing_metrics_without_losing_duration() {
    isolated(
        "backlog_forecast::arrivals_preserve_missing_metrics_without_losing_duration",
        || {
            // Two distinct history pools keep the initial tasks fully covered:
            // only the workspace-wide arrival stream can sample the gap.
            for (missing_tokens, missing_usd) in
                [(true, false), (false, true), (true, true), (false, false)]
            {
                let fixture = Fixture::new();
                let at = Utc::now() - Duration::days(2);
                let donor = completed_delivery(
                    &fixture,
                    at,
                    TaskComplexity::Medium,
                    "gpt-6.1-sol",
                    Some(10.0),
                    24,
                );
                super::backlog_rework::edit_task(&fixture, &donor, |task| {
                    task.tags.push("no-diff-expected".into())
                });
                let donor = completed_delivery(
                    &fixture,
                    at,
                    TaskComplexity::Low,
                    if missing_tokens {
                        "unpriced-fixture-model"
                    } else {
                        "gpt-6.1-sol"
                    },
                    if missing_usd { None } else { Some(10.0) },
                    24,
                );
                super::backlog_rework::edit_task(&fixture, &donor, |task| {
                    task.tags.push("no-diff-expected".into())
                });
                for status in [TaskStatus::Backlog, TaskStatus::Proposed] {
                    fixture
                        .runtime
                        .add_task(TaskAddParams {
                            title: "Covered initial work".into(),
                            complexity: TaskComplexity::Medium,
                            status: Some(status),
                            ..Default::default()
                        })
                        .unwrap();
                }
                let server = fixture.server(false);
                let off = initial_work_forecast_baseline(json_ok(
                    server.get(&format!("{ENDPOINT}&concurrency=1")),
                ));
                let on = initial_work_forecast_baseline(json_ok(
                    server.get(&format!("{ENDPOINT}&concurrency=1&arrivals=on")),
                ));
                for key in ["forecast", "if_approved"] {
                    for metric in ["time_to_drain_hours", "tokens", "usd"] {
                        for rank in ["p50", "p90"] {
                            assert!(
                                off[key][metric][rank]
                                    .as_f64()
                                    .is_some_and(|v| v.is_finite() && v > 0.0)
                            );
                        }
                    }
                    for (metric, missing) in [
                        ("time_to_drain_hours", false),
                        ("tokens", missing_tokens),
                        ("usd", missing_usd),
                    ] {
                        if missing {
                            assert!(
                                on[key][metric]["p90"].is_null(),
                                "sampled arrivals with missing {metric} must not count as free: {}",
                                on[key]
                            );
                        } else {
                            for rank in ["p50", "p90"] {
                                assert!(
                                    on[key][metric][rank]
                                        .as_f64()
                                        .is_some_and(|v| v.is_finite() && v > 0.0),
                                    "arrival telemetry gaps must not invalidate independent {metric}: {}",
                                    on[key]
                                );
                            }
                        }
                    }
                }
                assert_eq!(on["coverage"]["uncovered_model"], 0);
                assert_eq!(on["coverage"]["uncosted"], 0);
                assert_eq!(on["proposed_coverage"]["uncosted"], 0);
                // Arrival coverage is shared by backlog and if-approved totals,
                // and reported even when arrivals are switched off.
                let coverage = json!({"deliveries": 2, "missing_tokens": u64::from(missing_tokens), "missing_usd": u64::from(missing_usd)});
                for key in ["deliveries", "missing_tokens", "missing_usd"] {
                    assert_eq!(on["arrivals"]["coverage"][key], coverage[key]);
                    assert_eq!(off["arrivals"]["coverage"][key], coverage[key]);
                }
                assert_eq!(on["arrivals"]["coverage"]["conflict_donors"], 2);
            }
        },
    );
}

#[test]
fn thin_metric_support_is_disclosed_and_counted() {
    isolated(
        "backlog_forecast::thin_metric_support_is_disclosed_and_counted",
        || {
            // One measured delivery among five: the pool has five time samples,
            // but only one measured token and cost sample [ORB-21948].
            let fixture = Fixture::new();
            let at = Utc::now() - Duration::days(1);
            completed_delivery(
                &fixture,
                at,
                TaskComplexity::Medium,
                "gpt-6.1-sol",
                Some(1.0),
                1,
            );
            for _ in 0..4 {
                completed_delivery(
                    &fixture,
                    at,
                    TaskComplexity::Medium,
                    "unpriced-fixture-model",
                    None,
                    1,
                );
            }
            let queued = backlog_task(&fixture, "Thin metric work", &["file:thin.rs"], &[], &[]);
            let server = fixture.server(false);
            let forecast = initial_work_forecast_baseline(json_ok(server.get(ENDPOINT)));
            let row = prediction(&forecast, &queued);
            assert_eq!(row["token_samples"], 1, "{row}");
            assert_eq!(row["cost_samples"], 1, "{row}");
            assert_eq!(row["thin_tokens"], true, "{row}");
            assert_eq!(row["thin_cost"], true, "{row}");
            // One measured sample gives p50 == p90 by construction.
            assert_eq!(row["tokens"]["p50"], row["tokens"]["p90"], "{row}");
            assert_eq!(row["usd"]["p50"], row["usd"]["p90"], "{row}");
            assert_eq!(
                forecast["coverage"]["thin_tokens"], 1,
                "{}",
                forecast["coverage"]
            );
            assert_eq!(
                forecast["coverage"]["thin_cost"], 1,
                "{}",
                forecast["coverage"]
            );
            assert_eq!(forecast["coverage"]["uncosted"], 0);
            assert_eq!(forecast["coverage"]["uncovered_model"], 0);
            assert_eq!(forecast["inputs"]["min_metric_samples"], 5);
            let stored = fixture.runtime.backlog_forecasts(10).unwrap();
            assert_eq!(stored.len(), 1);
            let stored_tasks: Value = serde_json::from_str(&stored[0].tasks_json).unwrap();
            let stored_row = stored_tasks
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == queued)
                .unwrap();
            assert_eq!(stored_row["token_samples"], 1, "{stored_row}");
            assert_eq!(stored_row["cost_samples"], 1, "{stored_row}");
            assert_eq!(stored_row["thin_tokens"], true, "{stored_row}");
            assert_eq!(stored_row["thin_cost"], true, "{stored_row}");

            // Ample measured samples keep ranges with no thin marking.
            let fixture = Fixture::new();
            one_hour_history(&fixture);
            let queued = backlog_task(&fixture, "Ample metric work", &["file:ample.rs"], &[], &[]);
            let server = fixture.server(false);
            let forecast = initial_work_forecast_baseline(json_ok(server.get(ENDPOINT)));
            let row = prediction(&forecast, &queued);
            assert_eq!(row["token_samples"], 5, "{row}");
            assert_eq!(row["cost_samples"], 5, "{row}");
            assert_eq!(row["thin_tokens"], false, "{row}");
            assert_eq!(row["thin_cost"], false, "{row}");
            assert!(row["tokens"]["p50"].as_f64().unwrap() > 0.0, "{row}");
            assert!(row["usd"]["p50"].as_f64().unwrap() > 0.0, "{row}");
            assert_eq!(forecast["coverage"]["thin_tokens"], 0);
            assert_eq!(forecast["coverage"]["thin_cost"], 0);
        },
    );
}

/// `filings` tasks filed this week, a one-hour no-diff delivery as the only
/// history, and one task in the backlog.
fn busy_arrivals_fixture(fixture: &Fixture, filings: usize) {
    let donor = completed_delivery(
        fixture,
        Utc::now() - Duration::days(2),
        TaskComplexity::Medium,
        "gpt-6.1-sol",
        Some(1.0),
        1,
    );
    super::backlog_rework::edit_task(fixture, &donor, |task| {
        task.tags.push("no-diff-expected".into())
    });
    for _ in 0..filings {
        fixture
            .runtime
            .add_task(TaskAddParams {
                title: "Recent filing".into(),
                status: Some(TaskStatus::Done),
                ..Default::default()
            })
            .unwrap();
    }
    let queued = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Initial work".into(),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    super::backlog_rework::edit_task(fixture, &queued.id.to_string(), |task| {
        task.tags.push("no-diff-expected".into())
    });
}

#[test]
fn arrivals_past_the_generation_bound_report_the_cut_and_withhold_totals() {
    isolated(
        "backlog_forecast::arrivals_past_the_generation_bound_report_the_cut_and_withhold_totals",
        || {
            let fixture = Fixture::new();
            busy_arrivals_fixture(&fixture, 2_150);
            // Over 300 one-hour filings a day keep some task running at every
            // moment, so no run drains before the stream's limit. Which bound
            // ends the run must not depend on host speed: the production budget
            // starts before history loads, so a loaded host can spend it first and
            // report compute_budget. This read uses a budget no host reaches, as
            // the drain test below does; the production budget is read afterwards.
            let server = fixture.server_with_envs(
                false,
                &[(
                    orbit_core::test_support::BACKLOG_FORECAST_COMPUTE_BUDGET_ENV,
                    "3600000".as_ref(),
                )],
            );
            let cut = json_ok(server.get(&format!("{ENDPOINT}&concurrency=64&arrivals=on")));
            drop(server);
            assert!(cut["arrivals"]["per_day"]["total"].as_f64().unwrap() >= 300.0);
            assert_eq!(cut["inputs"]["iterations"], 1000);
            let forecast = &cut["forecast"];
            assert_eq!(
                forecast["simulation_cut"]["reason"], "generated_limit",
                "{forecast}"
            );
            assert_eq!(forecast["simulation_cut"]["generated_limit"], 10_000);
            assert_eq!(forecast["simulation_cut"]["iterations_completed"], 0);
            assert_eq!(forecast["unavailable"]["simulation_cut"], true);
            assert_eq!(forecast["simulation_truncated"], 1);
            assert!(
                forecast.get("pre_regression_baseline").is_none(),
                "{forecast}"
            );
            let unknown = json!({"p50": null, "p90": null});
            for metric in [
                "time_to_drain_hours",
                "tokens",
                "usd",
                "effective_parallelism",
                "critical_path_hours",
            ] {
                assert_eq!(forecast[metric], unknown, "{metric}");
            }
            for metric in ["work_hours", "tokens", "usd"] {
                assert_eq!(forecast["arrival_work"][metric], unknown, "{metric}");
            }
            assert!(forecast["drain_probability"].is_null(), "{forecast}");

            // The dashboard's read keeps its time limit under the production
            // budget. Whichever bound this host reaches first, the run is cut
            // and its totals are withheld.
            let server = fixture.server(false);
            let started = std::time::Instant::now();
            let read = json_ok(server.get(&format!("{ENDPOINT}&concurrency=64&arrivals=on")));
            let elapsed = started.elapsed();
            assert!(
                elapsed < std::time::Duration::from_secs(30),
                "the dashboard abandons this read at 30 seconds: {elapsed:?}"
            );
            let forecast = &read["forecast"];
            assert!(
                matches!(
                    forecast["simulation_cut"]["reason"].as_str(),
                    Some("generated_limit" | "compute_budget")
                ),
                "{forecast}"
            );
            assert_eq!(forecast["simulation_cut"]["iterations_completed"], 0);
            assert_eq!(forecast["unavailable"]["simulation_cut"], true);
            assert!(
                forecast.get("pre_regression_baseline").is_none(),
                "{forecast}"
            );
            let unknown = json!({"p50": null, "p90": null});
            for metric in ["time_to_drain_hours", "tokens", "usd"] {
                assert_eq!(forecast[metric], unknown, "{metric}");
            }
            assert!(forecast["drain_probability"].is_null(), "{forecast}");
        },
    );
}

#[test]
fn arrivals_past_the_compute_budget_report_the_cut_and_withhold_totals() {
    isolated(
        "backlog_forecast::arrivals_past_the_compute_budget_report_the_cut_and_withhold_totals",
        || {
            let fixture = Fixture::new();
            busy_arrivals_fixture(&fixture, 850);
            // A spent budget cuts before the first iteration whatever the
            // host's speed.
            let server = fixture.server_with_envs(
                false,
                &[(
                    orbit_core::test_support::BACKLOG_FORECAST_COMPUTE_BUDGET_ENV,
                    "0".as_ref(),
                )],
            );
            let cut = json_ok(server.get(&format!("{ENDPOINT}&concurrency=64&arrivals=on")));
            let forecast = &cut["forecast"];
            assert_eq!(
                forecast["simulation_cut"]["reason"], "compute_budget",
                "{forecast}"
            );
            assert_eq!(forecast["simulation_cut"]["iterations_completed"], 0);
            assert_eq!(forecast["simulation_cut"]["compute_budget_seconds"], 0);
            assert_eq!(forecast["unavailable"]["simulation_cut"], true);
            // The bound that cut the run is the budget, not the generation limit.
            assert_eq!(forecast["simulation_truncated"], 0);
            assert!(
                forecast.get("pre_regression_baseline").is_none(),
                "{forecast}"
            );
            let unknown = json!({"p50": null, "p90": null});
            for metric in ["time_to_drain_hours", "tokens", "usd"] {
                assert_eq!(forecast[metric], unknown, "{metric}");
            }
            assert!(forecast["drain_probability"].is_null(), "{forecast}");
        },
    );
}

#[test]
fn arrivals_draining_before_the_generation_bound_stay_complete() {
    isolated(
        "backlog_forecast::arrivals_draining_before_the_generation_bound_stay_complete",
        || {
            let fixture = Fixture::new();
            busy_arrivals_fixture(&fixture, 850);
            // The simulation costs well over the production wall-time budget
            // on a host slowed by instrumentation, so this checks completion
            // under a budget no host reaches; the budget's own cut is checked
            // by the test above.
            let server = fixture.server_with_envs(
                false,
                &[(
                    orbit_core::test_support::BACKLOG_FORECAST_COMPUTE_BUDGET_ENV,
                    "3600000".as_ref(),
                )],
            );
            // Over 90 days this rate files past the limit, yet every run
            // drains long before using that many filings.
            let complete = json_ok(server.get(&format!("{ENDPOINT}&concurrency=64&arrivals=on")));
            assert!(complete["arrivals"]["per_day"]["total"].as_f64().unwrap() > 111.2);
            let forecast = &complete["forecast"];
            assert!(forecast.get("simulation_cut").is_none(), "{forecast}");
            assert!(forecast["unavailable"].get("simulation_cut").is_none());
            assert_eq!(forecast["simulation_truncated"], 0);
            // This one-delivery history supports no regression estimate, so
            // the initial-work baseline carries the scenario's figures.
            let baseline = &forecast["pre_regression_baseline"];
            assert_eq!(baseline["drain_probability"], 1.0, "{forecast}");
            assert!(
                baseline["time_to_drain_hours"]["p90"].as_f64().is_some(),
                "{forecast}"
            );
        },
    );
}

/// The furthest apart two back-to-back forecast scans may be. Each live
/// forecast builds its own riskiness model from a fresh wall-clock scan, so
/// the two `observed_through` instants differ by the requests' own compute
/// time — one second across the F2026-11-097 CI run. The semantic remainder
/// must still match exactly, so this bound only recognizes same-test scans;
/// it matches the cross-scan skew the
/// `forecast_riskiness_preserves_observation_timestamps` regression allows.
const NO_ARRIVALS_SCAN_SKEW_SECONDS: i64 = 3600;

/// One riskiness observation window read from a forecast prediction.
struct RiskinessScanWindow {
    since: DateTime<Utc>,
    observed_through: DateTime<Utc>,
    window_days: u64,
}

/// Validate every `task_predictions[].riskiness` observation window in
/// `forecast` and remove `since`/`observed_through` from it, returning the
/// removed windows. The windows keep their exact RFC3339 instants (never
/// date-only: ORB-22032) and must each span `window_days`.
fn strip_riskiness_scan_windows(
    forecast: &mut Value,
    side: &str,
) -> Result<Vec<RiskinessScanWindow>, String> {
    if !forecast
        .get("task_predictions")
        .is_some_and(|predictions| predictions.is_array())
    {
        return Err(format!(
            "{side} forecast has no task_predictions array: {forecast}"
        ));
    }
    let rows = forecast
        .get_mut("task_predictions")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| format!("{side} forecast has no task_predictions array"))?;
    let mut windows = Vec::with_capacity(rows.len());
    for row in rows.iter_mut() {
        let id = row.get("id").unwrap_or(&Value::Null).clone();
        let row_context = row.to_string();
        let riskiness = row
            .get_mut("riskiness")
            .ok_or_else(|| format!("{side} prediction {id} has no riskiness: {row_context}"))?;
        let context = riskiness.to_string();
        let object = riskiness.as_object_mut().ok_or_else(|| {
            format!("{side} prediction {id} riskiness is not an object: {row_context}")
        })?;
        let raw = ["since", "observed_through"]
            .into_iter()
            .map(|field| {
                object
                    .get(field)
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        format!(
                            "{side} prediction {id} riskiness.{field} is not a string: {context}"
                        )
                    })
                    .map(|raw| (field, raw))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut instants = Vec::with_capacity(2);
        for (field, text) in raw {
            if !text.contains('T') {
                return Err(format!(
                    "{side} prediction {id} riskiness.{field} must retain the RFC3339 instant, \
                     not a date-only string ({text}): {context}"
                ));
            }
            let instant = DateTime::parse_from_rfc3339(text)
                .map_err(|_| {
                    format!(
                        "{side} prediction {id} riskiness.{field} must parse as RFC3339 ({text}): \
                         {context}"
                    )
                })?
                .with_timezone(&Utc);
            instants.push(instant);
        }
        let since = instants[0];
        let observed_through = instants[1];
        if since >= observed_through {
            return Err(format!(
                "{side} prediction {id} observation window must advance: {context}"
            ));
        }
        let window_days = object
            .get("window_days")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                format!("{side} prediction {id} riskiness has no window_days: {context}")
            })?;
        let span_hours = (observed_through - since).num_seconds() as f64 / 3600.0;
        if (span_hours - window_days as f64 * 24.0).abs() >= 1.0 {
            return Err(format!(
                "{side} prediction {id} riskiness window must span window_days ({window_days}): \
                 {context}"
            ));
        }
        object.remove("since");
        object.remove("observed_through");
        windows.push(RiskinessScanWindow {
            since,
            observed_through,
            window_days,
        });
    }
    Ok(windows)
}

/// Compare the no-arrivals (`off`) and arrivals-on (`on`) forecasts for
/// semantic equality, tolerating only independently sampled riskiness scan
/// instants. Every estimate, probability, cohort/prediction identity and
/// completeness field must match exactly; the excluded `since`/
/// `observed_through` pairs must each be an exact RFC3339 window from the
/// same estimator, sampled within same-test skew (F2026-11-097).
/// Returns the number of riskiness windows compared.
fn compare_no_arrivals_forecasts(off: &Value, on: &Value) -> Result<usize, String> {
    let mut off = off.clone();
    let mut on = on.clone();
    let off_windows = strip_riskiness_scan_windows(&mut off, "off")?;
    let on_windows = strip_riskiness_scan_windows(&mut on, "on")?;
    if off_windows.len() != on_windows.len() {
        return Err(format!(
            "off and on forecasts compare {} and {} riskiness windows",
            off_windows.len(),
            on_windows.len()
        ));
    }
    if off != on {
        return Err(format!("semantic forecast difference: off: {off} on: {on}"));
    }
    for (index, (off_window, on_window)) in off_windows.iter().zip(&on_windows).enumerate() {
        if off_window.window_days != on_window.window_days {
            return Err(format!(
                "prediction {index} compares window_days {} against {}",
                off_window.window_days, on_window.window_days
            ));
        }
        for (field, off_instant, on_instant) in [
            ("since", off_window.since, on_window.since),
            (
                "observed_through",
                off_window.observed_through,
                on_window.observed_through,
            ),
        ] {
            let skew = (off_instant - on_instant).num_seconds().abs();
            if skew >= NO_ARRIVALS_SCAN_SKEW_SECONDS {
                return Err(format!(
                    "prediction {index} {field} scans are {skew}s apart, beyond same-test skew"
                ));
            }
        }
    }
    Ok(off_windows.len())
}

#[test]
fn arrivals_on_with_no_recorded_arrivals_keeps_the_no_arrivals_forecast() {
    isolated(
        "backlog_forecast::arrivals_on_with_no_recorded_arrivals_keeps_the_no_arrivals_forecast",
        || {
            let fixture = Fixture::new();
            let at = Utc::now() - Duration::days(20);
            completed_work(&fixture, at, TaskComplexity::Medium, 2);
            let queued = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Initial work".into(),
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            super::backlog_rework::edit_task(&fixture, &queued.id.to_string(), |task| {
                task.created_at = at;
            });
            let server = fixture.server(false);
            let off = json_ok(server.get(&format!("{ENDPOINT}&concurrency=1&refresh=1")));
            let on = json_ok(server.get(&format!("{ENDPOINT}&concurrency=1&arrivals=on")));
            assert_eq!(on["arrivals"]["per_day"]["total"], 0.0);
            assert_eq!(on["inputs"]["arrivals"], true);
            let compared = compare_no_arrivals_forecasts(&off["forecast"], &on["forecast"])
                .unwrap_or_else(|diff| {
                    panic!(
                        "arrivals=on with zero arrivals must preserve the no-arrivals forecast: \
                         {diff}"
                    )
                });
            assert!(
                compared > 0,
                "the comparison must exercise at least one sampled riskiness window: {}",
                on["forecast"]["task_predictions"]
            );
            assert!(on["forecast"].get("simulation_cut").is_none());
        },
    );
}

/// A completed delivery of `hours` with provider cost equal to those hours.
pub(super) fn completed_work(
    fixture: &Fixture,
    at: DateTime<Utc>,
    complexity: TaskComplexity,
    hours: u64,
) -> String {
    completed_delivery(
        fixture,
        at,
        complexity,
        "gpt-6.1-sol",
        Some(hours as f64),
        hours,
    )
}

/// Move a [`completed_delivery`] invocation's end to `at`. Duration is unchanged.
pub(super) fn retimestamp_invocation(fixture: &Fixture, id: &str, at: DateTime<Utc>) {
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE invocations SET ts = ?1 WHERE job_run_id = ?2",
                    (at.to_rfc3339(), format!("delivery-{id}")),
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

/// Five identical one-hour medium deliveries, so every draw is one hour.
pub(super) fn one_hour_history(fixture: &Fixture) {
    for _ in 0..5 {
        completed(fixture, Utc::now() - Duration::days(1));
    }
}

pub(super) fn backlog_task(
    fixture: &Fixture,
    title: &str,
    context_files: &[&str],
    tags: &[&str],
    dependencies: &[&str],
) -> String {
    fixture
        .runtime
        .add_task(TaskAddParams {
            title: title.into(),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            context_files: context_files.iter().map(|path| path.to_string()).collect(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            dependencies: dependencies.iter().map(|id| id.to_string()).collect(),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string()
}

/// Initial-work fixtures have no fix history. Inspect the explicitly
/// labelled initial-work baseline; rework fixtures assert full totals.
pub(super) fn initial_work_forecast_baseline(mut response: Value) -> Value {
    for key in ["forecast", "if_approved"] {
        if response[key]["pre_regression_baseline"].is_object() {
            let predictions = response[key]["task_predictions"].take();
            response[key] = response[key]["pre_regression_baseline"].take();
            response[key]["task_predictions"] = predictions;
        }
    }
    response
}

pub(super) fn drain_hours(forecast: &Value) -> (f64, f64) {
    let hours = &forecast["forecast"]["time_to_drain_hours"];
    (
        hours["p50"].as_f64().unwrap(),
        hours["p90"].as_f64().unwrap(),
    )
}

pub(super) fn prediction<'a>(forecast: &'a Value, id: &str) -> &'a Value {
    forecast["forecast"]["task_predictions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap()
}

#[test]
fn dependency_chain_runs_in_sequence_despite_free_slots() {
    isolated(
        "backlog_forecast::dependency_chain_runs_in_sequence_despite_free_slots",
        || {
            let fixture = Fixture::new();
            one_hour_history(&fixture);
            let a = backlog_task(&fixture, "Chain A", &["file:a.rs"], &[], &[]);
            let b = backlog_task(&fixture, "Chain B", &["file:b.rs"], &[], &[&a]);
            let c = backlog_task(&fixture, "Chain C", &["file:c.rs"], &[], &[&b]);
            let server = fixture.server(false);
            let forecast = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&concurrency=4")),
            ));
            // Admission arrived in model 3; later sampling factors retain it.
            assert!(forecast["model_version"].as_u64().unwrap() >= 3);
            // Every draw is one hour, so every iteration drains in exactly the
            // chain's three hours: never in the one hour three free slots give.
            assert_eq!(
                drain_hours(&forecast),
                (3.0, 3.0),
                "{}",
                forecast["forecast"]
            );
            let parallelism = &forecast["forecast"]["effective_parallelism"];
            assert_eq!(parallelism["p50"], 1.0);
            assert_eq!(forecast["forecast"]["critical_path_hours"]["p90"], 3.0);
            for (id, start) in [(&a, 0.0), (&b, 1.0), (&c, 2.0)] {
                assert_eq!(
                    prediction(&forecast, id)["predicted_start_hours"]["p50"],
                    start
                );
            }
            assert_eq!(
                forecast["forecast"]["wait_hours"]["dependency"]["p50"], 3.0,
                "B waits one hour and C two on their dependencies"
            );
            assert_eq!(forecast["forecast"]["completion_probability"], 1.0);
        },
    );
}

#[test]
fn overlapping_locks_serialize_and_unknown_footprints_run_alone() {
    isolated(
        "backlog_forecast::overlapping_locks_serialize_and_unknown_footprints_run_alone",
        || {
            for (case, tasks, expected) in [
                // Overlapping editing surfaces never run side by side.
                (
                    "overlap",
                    vec![(vec!["file:src/lib.rs"], vec![]), (vec!["dir:src"], vec![])],
                    2.0,
                ),
                (
                    "disjoint",
                    vec![
                        (vec!["file:src/a.rs"], vec![]),
                        (vec!["file:src/b.rs"], vec![]),
                    ],
                    1.0,
                ),
                // No-diff work holds no lock, so overlapping no-diff tasks run
                // together.
                (
                    "no-diff",
                    vec![
                        (vec!["file:src/lib.rs"], vec!["no-diff-expected"]),
                        (vec!["file:src/lib.rs"], vec!["no-diff-expected"]),
                    ],
                    1.0,
                ),
                // Editing work with no footprint holds the whole tree.
                (
                    "no-footprint",
                    vec![
                        (vec![], vec![]),
                        (vec!["file:src/a.rs"], vec![]),
                        (vec!["file:src/b.rs"], vec![]),
                    ],
                    2.0,
                ),
            ] {
                let fixture = Fixture::new();
                one_hour_history(&fixture);
                for (index, (files, tags)) in tasks.iter().enumerate() {
                    backlog_task(&fixture, &format!("{case} {index}"), files, tags, &[]);
                }
                let server = fixture.server(false);
                let forecast = initial_work_forecast_baseline(json_ok(
                    server.get(&format!("{ENDPOINT}&concurrency=3")),
                ));
                assert_eq!(
                    drain_hours(&forecast),
                    (expected, expected),
                    "{case}: {}",
                    forecast["forecast"]
                );
            }
        },
    );
}

#[test]
fn queue_follows_the_drains_dispatch_order() {
    isolated(
        "backlog_forecast::queue_follows_the_drains_dispatch_order",
        || {
            let fixture = Fixture::new();
            one_hour_history(&fixture);
            let mut ids = Vec::new();
            for (title, priority) in [
                ("Oldest low", orbit_types::task::TaskPriority::Low),
                ("Medium", orbit_types::task::TaskPriority::Medium),
                ("Newest critical", orbit_types::task::TaskPriority::Critical),
                ("Newest high", orbit_types::task::TaskPriority::High),
            ] {
                ids.push(
                    fixture
                        .runtime
                        .add_task(TaskAddParams {
                            title: title.into(),
                            complexity: TaskComplexity::Medium,
                            status: Some(TaskStatus::Backlog),
                            context_files: vec![format!("file:{title}.rs")],
                            priority,
                            ..Default::default()
                        })
                        .unwrap()
                        .id
                        .to_string(),
                );
            }
            let server = fixture.server(false);
            let forecast = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&concurrency=1")),
            ));
            let order: Vec<&str> = forecast["forecast"]["task_predictions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["id"].as_str().unwrap())
                .collect();
            let expected = [&ids[2], &ids[3], &ids[1], &ids[0]];
            assert_eq!(order, expected, "critical, high, then medium and low");
            for (rank, id) in expected.iter().enumerate() {
                assert_eq!(
                    prediction(&forecast, id)["predicted_start_rank"]["p90"],
                    rank as f64 + 1.0
                );
            }
        },
    );
}

#[test]
fn unschedulable_dependency_withholds_full_totals_and_reports_a_subset() {
    isolated(
        "backlog_forecast::unschedulable_dependency_withholds_full_totals_and_reports_a_subset",
        || {
            let fixture = Fixture::new();
            one_hour_history(&fixture);
            let in_review = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Waiting for review".into(),
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::Review),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            let blocked =
                backlog_task(&fixture, "After review", &["file:a.rs"], &[], &[&in_review]);
            let downstream = backlog_task(&fixture, "Downstream", &["file:b.rs"], &[], &[&blocked]);
            backlog_task(&fixture, "Free", &["file:c.rs"], &[], &[]);
            let server = fixture.server(false);
            let forecast = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&concurrency=2")),
            ));
            let totals = &forecast["forecast"];
            assert!(totals["time_to_drain_hours"]["p50"].is_null(), "{totals}");
            assert!(totals["usd"]["p90"].is_null());
            assert!(totals["completion_probability"].is_null());
            assert_eq!(totals["schedulable_subset"]["tasks"], 1);
            assert_eq!(totals["schedulable_subset"]["of"], 3);
            assert_eq!(
                totals["schedulable_subset"]["time_to_drain_hours"]["p50"],
                1.0
            );
            let coverage = &forecast["coverage"];
            assert_eq!(coverage["dependency_unforecastable"], 2);
            assert_eq!(
                coverage["dependency_unforecastable_reasons"],
                json!({"review": 1, "upstream": 1})
            );
            assert_eq!(
                prediction(&forecast, &downstream)["unforecastable"],
                "upstream"
            );
            assert!(prediction(&forecast, &downstream)["predicted_start_hours"]["p50"].is_null());
        },
    );
}

#[test]
fn lock_blocked_critical_work_reserves_its_surface() {
    isolated(
        "backlog_forecast::lock_blocked_critical_work_reserves_its_surface",
        || {
            let fixture = Fixture::new();
            one_hour_history(&fixture);
            fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Running holder".into(),
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::InProgress),
                    context_files: vec!["file:x.rs".into()],
                    ..Default::default()
                })
                .unwrap();
            let critical = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Critical on x and y".into(),
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::Backlog),
                    context_files: vec!["file:x.rs".into(), "file:y.rs".into()],
                    priority: orbit_types::task::TaskPriority::Critical,
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            let low = backlog_task(&fixture, "Low on y", &["file:y.rs"], &[], &[]);
            let server = fixture.server(false);
            let forecast = initial_work_forecast_baseline(json_ok(
                server.get(&format!("{ENDPOINT}&concurrency=3")),
            ));
            // Without the reservation the low task would take y at once and
            // the critical task would wait for it too.
            assert_eq!(
                prediction(&forecast, &critical)["predicted_start_hours"]["p50"],
                1.0
            );
            assert_eq!(
                prediction(&forecast, &low)["predicted_start_hours"]["p50"],
                2.0
            );
            assert_eq!(
                forecast["forecast"]["wait_hours"]["reservation"]["p50"],
                1.0
            );
        },
    );
}
