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

const ENDPOINT: &str = "/api/diagnostics/backlog_forecast?workspace=ws_http_fixture";

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

fn forecast_tick(fixture: &Fixture, now: DateTime<Utc>, dry_run: bool) {
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

            let server = fixture.server(false);
            let read = json_ok(server.get(ENDPOINT));
            assert_eq!(read["stored"]["recorded"], false);
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
            // Per-task predictions are stored with each task, not twice.
            let mut read_forecast = read["forecast"].clone();
            let predictions = read_forecast
                .as_object_mut()
                .unwrap()
                .remove("task_predictions")
                .unwrap();
            assert_eq!(
                read_forecast,
                serde_json::from_str::<Value>(&rows[0].result_json).unwrap()
            );
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

fn history(
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
fn completed(fixture: &Fixture, at: DateTime<Utc>) -> String {
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

fn comment(fixture: &Fixture, id: &str) {
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
            assert_eq!(before["inputs"]["history_deliveries"], 1);
            assert_eq!(before["inputs"]["history_without_invocations"], 0);
            assert_eq!(before["arrivals"]["done_per_day"], 0.14);
            let calibration = &before["calibration"];
            assert_eq!(
                calibration["forecasts"][0]["actual"]["time_to_drain_hours"],
                1.0
            );
            assert_eq!(calibration["forecasts"][0]["scored_whole"], true);
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
                comment(&fixture, id);
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
                    after["inputs"]["history_deliveries"], 1,
                    "touching an old delivery must not move it into the lookback"
                );
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
                completed_delivery(
                    &fixture,
                    at,
                    TaskComplexity::Medium,
                    "gpt-6.1-sol",
                    Some(10.0),
                    24,
                );
                completed_delivery(
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
                let off = json_ok(server.get(&format!("{ENDPOINT}&concurrency=1")));
                let on = json_ok(server.get(&format!("{ENDPOINT}&concurrency=1&arrivals=on")));
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
                assert_eq!(on["arrivals"]["coverage"], coverage);
                assert_eq!(off["arrivals"]["coverage"], coverage);
            }
        },
    );
}

/// Five identical one-hour medium deliveries, so every draw is one hour.
fn one_hour_history(fixture: &Fixture) {
    for _ in 0..5 {
        completed(fixture, Utc::now() - Duration::days(1));
    }
}

fn backlog_task(
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

fn drain_hours(forecast: &Value) -> (f64, f64) {
    let hours = &forecast["forecast"]["time_to_drain_hours"];
    (
        hours["p50"].as_f64().unwrap(),
        hours["p90"].as_f64().unwrap(),
    )
}

fn prediction<'a>(forecast: &'a Value, id: &str) -> &'a Value {
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
            let forecast = json_ok(server.get(&format!("{ENDPOINT}&concurrency=4")));
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
                let forecast = json_ok(server.get(&format!("{ENDPOINT}&concurrency=3")));
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
            let forecast = json_ok(server.get(&format!("{ENDPOINT}&concurrency=1")));
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
            let forecast = json_ok(server.get(&format!("{ENDPOINT}&concurrency=2")));
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
            let forecast = json_ok(server.get(&format!("{ENDPOINT}&concurrency=3")));
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
