//! Analytics › Experiments authoring routes against the real server: only an
//! operator session may create, edit or start an experiment. Each write runs
//! core's lifecycle function, so core's refusals come back with core's text
//! and every write records one event with actor `dashboard` and the reason.

use std::fs;

use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, json_ok};

const LIST: &str = "/api/diagnostics/experiments?workspace=ws_http_fixture";

fn route(path: &str) -> String {
    format!("/api/diagnostics/experiments{path}?workspace=ws_http_fixture")
}

fn draft() -> Value {
    json!({
        "name": "trial",
        "hypothesis": "A second arm changes nothing measurable.",
        "arms": [
            { "name": "control", "control": true },
            { "name": "treatment", "weight": 2 },
        ],
        "population": { "complexity": ["medium"] },
        "reason": "first draft",
    })
}

fn refusal(response: reqwest::blocking::Response, status: u16) -> String {
    let actual = response.status().as_u16();
    let body: Value = response.json().expect("refusal JSON");
    assert_eq!(actual, status, "{body}");
    body["error"].as_str().expect("refusal text").to_string()
}

#[test]
fn experiment_authoring_routes_are_operator_only_and_record_core_events() {
    isolated(
        "experiments::experiment_authoring_routes_are_operator_only_and_record_core_events",
        || {
            let fixture = Fixture::new();
            {
                let agent = fixture.server(false);
                let list = json_ok(agent.get(LIST));
                assert_eq!(
                    list["authoring"]["create"]["authorized"],
                    json!(false),
                    "{list}"
                );
                for (method, path) in [
                    ("POST", route("")),
                    ("PATCH", route("/trial")),
                    ("POST", route("/trial/start")),
                ] {
                    let denied = error_code(
                        agent.send(method, &path, draft()),
                        403,
                        "authorization_denied",
                    );
                    assert!(
                        denied["operation"]
                            .as_str()
                            .is_some_and(|id| id.starts_with("experiment.")),
                        "{method} {path}: {denied}"
                    );
                }
                assert_eq!(json_ok(agent.get(LIST))["experiments"], json!([]));
            }

            let server = fixture.server(true);
            let list = json_ok(server.get(LIST));
            for action in ["create", "update", "start"] {
                assert_eq!(
                    list["authoring"][action]["authorized"],
                    json!(true),
                    "{list}"
                );
            }
            assert!(
                list["authoring"]["metrics"]
                    .as_array()
                    .is_some_and(|metrics| metrics.contains(&json!("first_pass_acceptance"))),
                "{list}"
            );

            let created = json_ok(server.send("POST", &route(""), draft()));
            assert_eq!(created["status"], json!("draft"), "{created}");
            assert_eq!(created["arms"][1]["weight"], json!(2), "{created}");
            let id = created["id"].as_str().expect("experiment id").to_string();
            let one = route(&format!("/{id}"));
            let start = route(&format!("/{id}/start"));

            let unknown = refusal(
                server.send("PATCH", &one, json!({ "primary_metric": "speed" })),
                400,
            );
            assert!(
                unknown
                    .starts_with("`primary_metric` names unknown metric `speed`; expected one of "),
                "core's refusal text: {unknown}"
            );
            let updated = json_ok(server.send(
                "PATCH",
                &one,
                json!({ "primary_metric": "first_pass_acceptance", "reason": "pick a metric" }),
            ));
            assert_eq!(updated["primary_metric"], json!("first_pass_acceptance"));

            let disabled = server.send("POST", &start, json!({ "reason": "go" }));
            assert_eq!(disabled.status().as_u16(), 409);
            let disabled: Value = disabled.json().unwrap();
            assert_eq!(
                disabled["error"],
                json!(
                    "experiment `trial` cannot start: experiments are disabled in this workspace (`experiments.enabled` is false)"
                ),
                "{disabled}"
            );

            let config = fixture.work.join("config.toml");
            let mut text = fs::read_to_string(&config).unwrap_or_default();
            text.push_str("\n[experiments]\nenabled = true\n");
            fs::write(&config, text).unwrap();
            let started = json_ok(server.send("POST", &start, json!({ "reason": "go" })));
            assert_eq!(started["status"], json!("running"), "{started}");

            let locked = refusal(
                server.send("PATCH", &one, json!({ "hypothesis": "x" })),
                400,
            );
            assert!(
                locked.starts_with("experiment `trial` is running: its arms, overlays, population, metrics and sample rules are editable only in draft"),
                "core's refusal text: {locked}"
            );

            let shown = json_ok(server.get(&one));
            let events = shown["events"].as_array().expect("events");
            let summary = events
                .iter()
                .map(|event| {
                    (
                        event["kind"].as_str().unwrap_or_default(),
                        event["actor"].as_str().unwrap_or_default(),
                        event["reason"].as_str().unwrap_or_default(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                summary,
                [
                    ("created", "dashboard", "first draft"),
                    ("updated", "dashboard", "pick a metric"),
                    ("started", "dashboard", "go"),
                ],
                "one event per accepted write, refusals record none: {shown}"
            );
        },
    );
}

/// A per-line primary's cumulative series is the ratio of accumulated
/// numerators over accumulated reviewable lines, not a blank series and
/// not a mean of per-task ratios; zero-line and unavailable tasks are out.
#[test]
fn experiment_cumulative_series_is_ratio_of_sums_for_per_line_primary() {
    isolated(
        "experiments::experiment_cumulative_series_is_ratio_of_sums_for_per_line_primary",
        || {
            use orbit_core::application::experiment::{
                ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
            };
            use orbit_core::application::task::TaskAddParams;
            use orbit_core::{TaskComplexity, TaskStatus, TaskType};
            use orbit_store::contracts::{
                DiffClassCounts, ExperimentAssignment, ExperimentStoreBackend, LandedDiffSource,
                LandedDiffStats, LandedDiffStoreBackend, ReviewableLines,
            };
            use orbit_types::telemetry::{InvocationTrace, TokenUsage};

            let fixture = Fixture::new();
            let config = fixture.work.join("config.toml");
            let mut text = fs::read_to_string(&config).unwrap_or_default();
            text.push_str("\n[experiments]\nenabled = true\n");
            fs::write(&config, text).unwrap();
            let runtime = &fixture.runtime;
            let actor = ExperimentActor {
                actor: "per-line-cumulative-test".into(),
                reason: None,
            };
            let experiment = runtime
                .create_experiment(
                    "per-line-cumulative",
                    ExperimentDefinitionInput {
                        arms: Some(json!([
                            { "name": "control", "control": true, "weight": 1 },
                            { "name": "treatment", "weight": 1 }
                        ])),
                        primary_metric: Some("cost_per_line".into()),
                        guardrails: Some(json!({"lines_changed": "+0"})),
                        min_sample: Some(2),
                        ..ExperimentDefinitionInput::default()
                    },
                    actor.clone(),
                )
                .unwrap();
            runtime
                .change_experiment(&experiment.id, ExperimentChange::Start, actor)
                .unwrap();

            let mut seq = 0u64;
            let mut measured = |arm: &str, cost: f64, lines: u64| {
                seq += 1;
                let task = runtime
                    .add_task(TaskAddParams {
                        title: format!("per-line cumulative {arm}"),
                        complexity: TaskComplexity::Low,
                        task_type: Some(TaskType::Chore),
                        status: Some(TaskStatus::Done),
                        ..TaskAddParams::default()
                    })
                    .unwrap();
                let workspace_id = runtime.workspace_id().unwrap();
                runtime
                    .sqlite_store()
                    .unwrap()
                    .assign_experiments(&workspace_id, &task.id, true, true, &|records, _, _| {
                        let record = records
                            .iter()
                            .find(|record| record.experiment.id == experiment.id)
                            .unwrap();
                        let arm_row = record.arms.iter().find(|row| row.name == arm).unwrap();
                        Ok(vec![ExperimentAssignment {
                            experiment: experiment.id.clone(),
                            task_id: task.id.to_string(),
                            arm: Some(arm.into()),
                            segment: Some(1),
                            digest: Some(arm_row.overlay_digest.clone()),
                            assigned_at: chrono::Utc::now(),
                            forced: false,
                            exclusion_reason: None,
                        }])
                    })
                    .unwrap();
                runtime
                    .stores()
                    .jobs()
                    .insert_job_run(
                        "task_local_pipeline",
                        1,
                        chrono::Utc::now(),
                        Some(json!({ "task_ids": [task.id.to_string()] })),
                        None,
                    )
                    .unwrap();
                runtime
                    .insert_invocation_trace_record(
                        &orbit_store::contracts::InvocationInsertParams {
                            agent_execution_id: None,
                            job_run_id: format!("fixture-{}", task.id),
                            account: None,
                            activity_id: "agent_implement".into(),
                            agent: "codex".into(),
                            provider: None,
                            model: Some("gpt-6.1-sol".into()),
                            task_ids: vec![task.id.to_string()],
                            trace: InvocationTrace {
                                usage: TokenUsage::default(),
                                provider_cost_usd: Some(cost),
                                ..InvocationTrace::default()
                            },
                        },
                    )
                    .unwrap();
                runtime
                    .sqlite_store()
                    .unwrap()
                    .record_landed_diff_stats(&LandedDiffStats {
                        workspace_id,
                        task_id: task.id.to_string(),
                        base_commit: format!("{:040x}", seq * 2),
                        landed_commit: format!("{:040x}", seq * 2 + 1),
                        landed_at: chrono::Utc::now(),
                        recorded_at: chrono::Utc::now(),
                        job_run_id: None,
                        source: LandedDiffSource::Landing,
                        crew: None,
                        model: None,
                        complexity: None,
                        task_type: None,
                        code: DiffClassCounts {
                            added: lines,
                            deleted: 0,
                            files: 1,
                        },
                        tests: DiffClassCounts::default(),
                        other: DiffClassCounts::default(),
                        reviewable: Some(ReviewableLines {
                            added: lines,
                            deleted: 0,
                        }),
                    })
                    .unwrap();
                task.id.to_string()
            };
            // Control 10/100 + 30/50 = 40/150; treatment 2/10 + 9/30 = 11/40.
            measured("control", 10.0, 100);
            measured("control", 30.0, 50);
            measured("treatment", 2.0, 10);
            measured("treatment", 9.0, 30);
            measured("control", 99.0, 0);

            let server = fixture.server(false);
            let body = json_ok(server.get(&route(&format!("/{}/readout", experiment.id))));
            let cumulative = &body["cumulative"];
            assert_eq!(cumulative["metric"], json!("cost_per_line"), "{body}");
            for (arm, want) in [("control", 40.0 / 150.0), ("treatment", 11.0 / 40.0)] {
                let points = cumulative["arms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|entry| entry["arm"] == json!(arm))
                    .unwrap()["points"]
                    .as_array()
                    .unwrap()
                    .clone();
                let last = points.last().unwrap();
                assert_eq!(last["n"], json!(2), "{arm}: {cumulative}");
                let value = last["value"].as_f64().expect("non-null cumulative value");
                assert!((value - want).abs() < 1e-9, "{arm}: {value} != {want}");
            }
        },
    );
}
