//! Completion dates survive terminal metadata writes at the public forecast boundary.

use chrono::{DateTime, Duration, Utc};
use orbit_core::StoredBacklogForecast;
use orbit_core::application::task::TaskAddParams;
use orbit_store::contracts::{InvocationInsertParams, TaskHistoryUpdateParams};
use orbit_types::task::{TaskComment, TaskComplexity, TaskEnvelopeV2, TaskEventRowV2, TaskStatus};
use orbit_types::telemetry::InvocationTrace;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

const ENDPOINT: &str = "/api/diagnostics/backlog_forecast?workspace=ws_http_fixture";

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
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Historical delivery".into(),
            complexity: TaskComplexity::Medium,
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
            model: Some("gpt-6.1-sol".into()),
            task_ids: vec![id.clone()],
            trace: InvocationTrace {
                duration_ms: 3_600_000,
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
                    (at.to_rfc3339(), format!("delivery-{id}")),
                )
                .map_err(|e| orbit_core::OrbitError::Store(e.to_string()))?;
            Ok(())
        })
        .unwrap();
    let index = rusqlite::Connection::open(fixture.global.join("tasks/index.sqlite")).unwrap();
    assert_eq!(index.execute(
        "UPDATE task_bundle_index SET updated_at = ?1, terminal_month = ?2 WHERE task_id = ?3",
        (at.to_rfc3339(), at.format("%Y-%m").to_string(), &id),
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
            assert_eq!(
                calibration["forecast_level"]["time_to_drain_hours"],
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
