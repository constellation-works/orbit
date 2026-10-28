//! Regression costs must be visible at the public forecast boundary.

use super::backlog_forecast::{ENDPOINT, backlog_task, completed};
use super::support::{Fixture, isolated, json_ok};
use chrono::{Duration, Utc};
use orbit_types::task::{TaskEnvelopeV2, TaskRelation, TaskRelationType};
use serde_json::Value;

pub(super) fn edit_task(fixture: &Fixture, id: &str, edit: impl FnOnce(&mut TaskEnvelopeV2)) {
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &fixture.runtime.global_root(),
        ),
    )
    .unwrap();
    let path = registry
        .find_task_binding(id)
        .unwrap()
        .unwrap()
        .canonical_path
        .join("task.yaml");
    let mut envelope: TaskEnvelopeV2 =
        serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut envelope);
    std::fs::write(path, serde_yaml::to_string(&envelope).unwrap()).unwrap();
}

fn link(target: &str) -> TaskRelation {
    TaskRelation {
        relation_type: TaskRelationType::RegressionFrom,
        target: target.into(),
        regression: None,
    }
}

const WIDE: [&str; 3] = ["file:risky.rs", "file:risky-a.rs", "file:risky-b.rs"];

fn measured_history(fixture: &Fixture, lag: Duration, missing_usd: bool) {
    seeded_history(fixture, lag, missing_usd, false);
}

/// Five regressed origins, five clean tasks and five fixes. `sized` varies
/// their footprints so riskiness, which fits size alone, has a slope:
/// three-file tasks regress more often (4 of 9) than one-file tasks (1 of
/// 6), with both outcomes at each size. Otherwise every task declares one
/// file.
fn seeded_history(fixture: &Fixture, lag: Duration, missing_usd: bool, sized: bool) {
    let now = Utc::now();
    for index in 0..5 {
        let risky = completed(fixture, now - Duration::days(2) - lag);
        edit_task(fixture, &risky, |task| {
            task.context_files = if index == 0 || !sized {
                vec!["file:risky.rs".into()]
            } else {
                WIDE.iter().map(|path| (*path).into()).collect()
            }
        });
        let safe = completed(fixture, now - Duration::days(2));
        edit_task(fixture, &safe, |task| {
            task.context_files = vec!["file:safe.rs".into()]
        });
        let fix = completed(fixture, now - Duration::days(1));
        edit_task(fixture, &fix, |task| {
            let suffixes: &[&str] = if sized { &["", "-a", "-b"] } else { &[""] };
            task.context_files = suffixes
                .iter()
                .map(|suffix| format!("file:fix-{index}{suffix}.rs"))
                .collect();
            task.created_at = now - Duration::days(2);
            task.relations = vec![link(&risky)];
        });
        if missing_usd {
            fixture
                .runtime
                .sqlite_store()
                .unwrap()
                .with_transaction(|tx| {
                    tx.connection()
                        .execute(
                            "UPDATE invocations SET provider_cost_usd = NULL WHERE job_run_id = ?1",
                            [format!("delivery-{fix}")],
                        )
                        .map_err(|e| orbit_core::OrbitError::Store(e.to_string()))?;
                    Ok(())
                })
                .unwrap();
        }
    }
}

fn risk_forecast(paths: &[&str], lag: Duration, missing_usd: bool) -> Value {
    sized_risk_forecast(paths, lag, missing_usd, false)
}

fn sized_risk_forecast(paths: &[&str], lag: Duration, missing_usd: bool, sized: bool) -> Value {
    let fixture = Fixture::new();
    seeded_history(&fixture, lag, missing_usd, sized);
    for _ in 0..3 {
        backlog_task(&fixture, "Future work", paths, &[], &[]);
    }
    let server = fixture.server(false);
    json_ok(server.get(&format!("{ENDPOINT}&concurrency=1")))
}

#[test]
fn regression_draws_add_cost_time_and_count_prior_fallbacks() {
    isolated(
        "backlog_rework::regression_draws_add_cost_time_and_count_prior_fallbacks",
        || {
            let high = sized_risk_forecast(&WIDE, Duration::hours(1), false, true);
            let low = sized_risk_forecast(&["file:safe.rs"], Duration::hours(1), false, true);
            assert!(high["model_version"].as_u64().unwrap() > 7);
            assert_eq!(high["inputs"]["admission_factors"]["regression_rework"], 1);
            assert_eq!(high["coverage"]["risk_prior_fallbacks"], 0);
            for metric in ["time_to_drain_hours", "usd", "tokens"] {
                assert!(
                    high["forecast"][metric]["p50"].as_f64().unwrap()
                        > low["forecast"][metric]["p50"].as_f64().unwrap(),
                    "{metric}: high={} low={}",
                    high["forecast"],
                    low["forecast"]
                );
            }
            assert!(
                high["forecast"]["regression_rework"]["expected_followups"]
                    .as_f64()
                    .unwrap()
                    > low["forecast"]["regression_rework"]["expected_followups"]
                        .as_f64()
                        .unwrap()
            );
            assert_eq!(high["forecast"]["regression_rework"]["generation_limit"], 1);
            let prior = risk_forecast(&[], Duration::hours(1), false);
            assert_eq!(prior["coverage"]["risk_prior_fallbacks"], 3);
            assert_eq!(prior["coverage"]["risk_unestimated"], 0);
            assert!(
                prior["forecast"]["regression_rework"]["expected_followups"]
                    .as_f64()
                    .unwrap()
                    > 0.0
            );
            assert!(prior["forecast"]["task_predictions"][0]["riskiness"]["probability"].is_null());
            assert_eq!(prior["arrivals"]["excluded_regression_filings"], 5);
        },
    );
}

#[test]
fn regression_detection_lags_and_unknown_costs_are_not_discarded() {
    isolated(
        "backlog_rework::regression_detection_lags_and_unknown_costs_are_not_discarded",
        || {
            let delayed = risk_forecast(&["file:risky.rs"], Duration::days(10), false);
            assert!(
                delayed["forecast"]["time_to_drain_hours"]["p50"]
                    .as_f64()
                    .unwrap()
                    >= 240.0
            );
            let outside = risk_forecast(&["file:risky.rs"], Duration::days(100), false);
            // The old origins are outside the risk window, so these tasks use a
            // supported workspace prior, without inventing a UI estimate.
            assert!(
                outside["forecast"]["regression_rework"]["detection_outside_horizon"]
                    .as_f64()
                    .unwrap()
                    > 0.0
            );
            assert_eq!(
                outside["forecast"]["regression_rework"]["expected_followups"],
                0.0
            );
            assert_eq!(outside["forecast"]["usd"]["p90"], 3.0);
            let missing = risk_forecast(&["file:risky.rs"], Duration::hours(1), true);
            assert!(missing["forecast"]["usd"]["p90"].is_null());
            assert!(missing["forecast"]["tokens"]["p90"].as_f64().unwrap() > 0.0);
        },
    );
}

#[test]
fn regression_bundles_keep_multiplicity_and_donor_lock_contention() {
    isolated(
        "backlog_rework::regression_bundles_keep_multiplicity_and_donor_lock_contention",
        || {
            let mut forecasts = Vec::new();
            for conflicting in [false, true] {
                let fixture = Fixture::new();
                measured_history(&fixture, Duration::hours(1), false);
                // Each observed regression required two fixes. They either
                // edit the same surface, or can hold two slots together.
                let fixes: Vec<_> = fixture
                    .runtime
                    .list_task_metadata()
                    .unwrap()
                    .into_iter()
                    .filter(|task| {
                        task.relations
                            .iter()
                            .any(|link| link.relation_type == TaskRelationType::RegressionFrom)
                    })
                    .collect();
                assert_eq!(fixes.len(), 5);
                for (index, first) in fixes.iter().enumerate() {
                    let second = completed(&fixture, Utc::now() - Duration::days(1));
                    edit_task(&fixture, &second, |task| {
                        task.relations = first.relations.clone();
                        task.created_at = first.created_at;
                        task.context_files = vec![if conflicting {
                            "file:repair.rs".into()
                        } else {
                            format!("file:second-{index}.rs")
                        }];
                    });
                    if conflicting {
                        edit_task(&fixture, &first.id, |task| {
                            task.context_files = vec!["file:repair.rs".into()]
                        });
                    }
                }
                backlog_task(&fixture, "Origin", &["file:risky.rs"], &[], &[]);
                let server = fixture.server(false);
                forecasts.push(json_ok(server.get(&format!("{ENDPOINT}&concurrency=4"))));
            }
            let parallel = &forecasts[0]["forecast"];
            let serial = &forecasts[1]["forecast"];
            assert_eq!(parallel["regression_rework"]["followups"]["p90"], 2.0);
            assert_eq!(serial["regression_rework"]["work_hours"]["p90"], 2.0);
            assert!(
                serial["time_to_drain_hours"]["p90"].as_f64().unwrap()
                    > parallel["time_to_drain_hours"]["p90"].as_f64().unwrap()
            );
            assert!(serial["wait_hours"]["lock"]["p90"].as_f64().unwrap() > 0.0);
            assert_eq!(serial["regression_rework"]["generation_limit"], 1);
        },
    );
}

#[test]
fn known_proposed_fix_only_suppresses_rework_in_the_approved_scenario() {
    isolated(
        "backlog_rework::known_proposed_fix_only_suppresses_rework_in_the_approved_scenario",
        || {
            let fixture = Fixture::new();
            measured_history(&fixture, Duration::hours(1), false);
            let origin = backlog_task(&fixture, "Origin", &["file:risky.rs"], &[], &[]);
            fixture
                .runtime
                .add_task(orbit_core::application::task::TaskAddParams {
                    title: "Known proposed fix".into(),
                    complexity: orbit_types::task::TaskComplexity::Medium,
                    status: Some(orbit_types::task::TaskStatus::Proposed),
                    context_files: vec!["file:safe.rs".into()],
                    relations: vec![link(&origin)],
                    ..Default::default()
                })
                .unwrap();
            let server = fixture.server(false);
            let response = json_ok(server.get(&format!("{ENDPOINT}&concurrency=1")));
            let base = response["forecast"]["regression_rework"]["expected_followups"]
                .as_f64()
                .unwrap();
            let approved = response["if_approved"]["regression_rework"]["expected_followups"]
                .as_f64()
                .unwrap();
            assert!(
                base > 0.0,
                "unapproved work cannot suppress a simulated fix"
            );
            assert!(
                approved < base,
                "the known initial fix suppresses its copy; that real fix still has its own risk"
            );
            assert_eq!(response["coverage"]["tasks"], 1);
            assert_eq!(response["proposed_coverage"]["tasks"], 1);
        },
    );
}

#[test]
fn unsupported_fix_footprints_do_not_make_the_bundle_cheaper() {
    isolated(
        "backlog_rework::unsupported_fix_footprints_do_not_make_the_bundle_cheaper",
        || {
            let fixture = Fixture::new();
            measured_history(&fixture, Duration::hours(1), false);
            let fix = fixture
                .runtime
                .list_task_metadata()
                .unwrap()
                .into_iter()
                .find(|task| !task.relations.is_empty())
                .unwrap();
            edit_task(&fixture, &fix.id, |task| {
                task.context_files = vec!["module:unresolved".into()]
            });
            backlog_task(&fixture, "Origin", &["file:risky.rs"], &[], &[]);
            let server = fixture.server(false);
            let forecast = json_ok(server.get(ENDPOINT));
            assert_eq!(forecast["coverage"]["regression_history_missing"], 1);
            assert_eq!(
                forecast["forecast"]["regression_rework"]["available"],
                false
            );
            assert!(forecast["forecast"]["usd"]["p50"].is_null());
            assert_eq!(
                forecast["inputs"]["regression_rework"]["missing_fix_history"],
                1
            );
        },
    );
}

#[test]
fn missing_regression_evidence_is_not_free_work() {
    isolated(
        "backlog_rework::missing_regression_evidence_is_not_free_work",
        || {
            let fixture = Fixture::new();
            completed(&fixture, Utc::now() - Duration::days(1));
            backlog_task(&fixture, "Unknown risk", &[], &[], &[]);
            let server = fixture.server(false);
            let forecast = json_ok(server.get(ENDPOINT));
            assert_eq!(forecast["coverage"]["risk_unestimated"], 1);
            assert!(forecast["forecast"]["time_to_drain_hours"]["p50"].is_null());
            assert_eq!(
                forecast["forecast"]["pre_regression_baseline"]["time_to_drain_hours"]["p50"],
                1.0
            );
        },
    );
}

#[test]
fn rework_calibration_waits_for_the_horizon_and_keeps_mature_snapshots() {
    isolated(
        "backlog_rework::rework_calibration_waits_for_the_horizon_and_keeps_mature_snapshots",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let snapshot = now - Duration::days(100);
            let origin = completed(&fixture, snapshot + Duration::days(1));
            let fix = completed(&fixture, snapshot + Duration::days(5));
            edit_task(&fixture, &fix, |task| {
                task.relations = vec![link(&origin)];
                task.created_at = snapshot + Duration::days(4);
            });
            let stored_tasks =
                serde_json::json!([{"id": origin, "complexity": "medium", "forecast": {}}]);
            let predicted = serde_json::json!({"time_to_drain_hours": {"p50": 120, "p90": 120}, "usd": {"p50": 2, "p90": 2}, "regression_rework": {"followups": {"p50": 1, "p90": 1}, "work_hours": {"p50": 1, "p90": 1}, "usd": {"p50": 1, "p90": 1}}});
            let row = |at, version| orbit_core::StoredBacklogForecast {
                workspace_id: String::new(),
                forecast_at: at,
                model_version: version,
                inputs_json: "{}".into(),
                result_json: predicted.to_string(),
                tasks_json: stored_tasks.to_string(),
            };
            assert!(
                fixture
                    .runtime
                    .record_backlog_forecast(row(snapshot, 9), Duration::zero())
                    .unwrap()
            );
            // Push the mature row out of the recent-60 read.
            for hour in 0..65 {
                fixture
                    .runtime
                    .record_backlog_forecast(
                        row(now - Duration::days(4) + Duration::hours(hour), 1),
                        Duration::zero(),
                    )
                    .unwrap();
            }
            fixture
                .runtime
                .record_backlog_forecast(row(now - Duration::hours(1), 9), Duration::zero())
                .unwrap();
            let server = fixture.server(false);
            let forecast = json_ok(server.get(ENDPOINT));
            let calibration = &forecast["calibration"];
            assert_eq!(calibration["mature_forecasts"], 1);
            let rows = calibration["forecasts"].as_array().unwrap();
            let mature = rows
                .iter()
                .find(|row| row["model_version"] == 9 && row["provisional"] == false)
                .unwrap();
            assert_eq!(mature["scored_whole"], true);
            assert_eq!(mature["actual"]["usd"], 2.0);
            assert_eq!(mature["actual"]["time_to_drain_hours"], 120.0);
            assert_eq!(mature["actual"]["regression_rework"]["followups"], 1);
            assert_eq!(mature["actual"]["regression_rework"]["usd"], 1.0);
            let young = rows
                .iter()
                .find(|row| row["model_version"] == 9 && row["provisional"] == true)
                .unwrap();
            assert_eq!(young["scored_whole"], false);
            assert!(young["actual"].is_null());
            let version = calibration["by_model_version"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["model_version"] == 9)
                .unwrap();
            assert_eq!(version["regression_rework"]["followups"]["n"], 1);
        },
    );
}
