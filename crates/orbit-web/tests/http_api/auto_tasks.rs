use std::collections::BTreeMap;
use std::time::Instant;

use chrono::{Duration, Utc};
use orbit_core::application::auto_tasks::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{AutoTaskAddParams, Task};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, auto_task_tag};
use serde_json::json;

use super::support::{Fixture, isolated, json_ok, write_json};

const URL: &str = "/api/auto-tasks?workspace=ws_http_fixture";

fn definition(fixture: &Fixture, name: &str) {
    definition_with_schedule(
        fixture,
        name,
        AutoTaskSchedule::Interval { every_minutes: 60 },
    );
}

fn definition_with_schedule(fixture: &Fixture, name: &str, schedule: AutoTaskSchedule) {
    fixture
        .runtime
        .auto_task_add(AutoTaskAddParams {
            name: name.into(),
            description: "HTTP instance projection fixture".into(),
            schedule,
            template: AutoTaskTemplate {
                title: format!("Fixture {name}"),
                description: "Recurring fixture".into(),
                acceptance_criteria: vec![],
                task_type: TaskType::Chore,
                tags: vec![],
                required_tools: vec![],
                context_files: vec![],
                priority: TaskPriority::Medium,
                complexity: None,
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap();
}

fn instance(fixture: &Fixture, tags: Vec<String>, status: TaskStatus) -> Task {
    let task = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Instance fixture".into(),
            description: "Task body not needed by the instance projection".repeat(50),
            tags,
            status: Some(if status == TaskStatus::Archived {
                TaskStatus::Done
            } else {
                status
            }),
            ..Default::default()
        })
        .unwrap();
    if status == TaskStatus::Archived {
        fixture.runtime.archive_task(&task.id).unwrap();
        fixture.runtime.get_task(&task.id).unwrap()
    } else {
        task
    }
}

fn seed(fixture: &Fixture, total: usize) -> BTreeMap<String, Vec<Task>> {
    let mut instances = BTreeMap::new();
    for index in 0..16 {
        let name = format!("fixture-{index:02}");
        let schedule = if index % 2 == 0 {
            AutoTaskSchedule::Interval { every_minutes: 60 }
        } else {
            serde_json::from_value(json!({"deliveries_landed": {
                "branch":"agent-main", "threshold":1, "max_wait_minutes":10,
                "coverage":"landed_code_review_v1", "max_items":50, "retries":0,
            }}))
            .unwrap()
        };
        definition_with_schedule(fixture, &name, schedule);
        let tasks = (0..20)
            .map(|_| instance(fixture, vec![auto_task_tag(&name)], TaskStatus::Backlog))
            .collect();
        instances.insert(name, tasks);
    }
    for _ in 320..total {
        instance(fixture, vec!["unrelated".into()], TaskStatus::Done);
    }
    instances
}

#[test]
fn list_uses_one_freshness_pass_and_selects_each_instance_once() {
    isolated(
        "auto_tasks::list_uses_one_freshness_pass_and_selects_each_instance_once",
        || {
            let fixture = Fixture::new();
            let instances = seed(&fixture, 320);
            let server = fixture.counted_task_server();
            for request in 1..=2 {
                let response = json_ok(server.get(URL));
                assert_eq!(response["definitions"].as_array().unwrap().len(), 16);
                for row in response["definitions"].as_array().unwrap() {
                    let newest = instances[row["name"].as_str().unwrap()].last().unwrap();
                    assert_eq!(row["last_minted_task_id"], json!(newest.id));
                    assert_eq!(row["last_minted_task_status"], json!(TaskStatus::Backlog));
                    assert_eq!(row["open_duplicate"], json!(true));
                    assert_eq!(row["may_create_open_duplicate"], json!(true));
                }
                let trace = server.task_query_trace();
                let mut selections = BTreeMap::<String, usize>::new();
                let mut freshness = 0;
                let mut bundles = 0;
                for event in trace {
                    match event["span"]["name"].as_str() {
                        Some("task_index_freshness") => freshness += 1,
                        Some("task_envelope_selection") => {
                            *selections
                                .entry(event["span"]["task_id"].as_str().unwrap().into())
                                .or_default() += 1;
                        }
                        Some("task_bundle_materialization") => bundles += 1,
                        _ => {}
                    }
                }
                assert_eq!(freshness, request, "one freshness pass on every refresh");
                assert_eq!(
                    bundles, 0,
                    "metadata projection must not hydrate task bodies"
                );
                assert_eq!(selections.len(), 320);
                for task in instances.values().flatten() {
                    assert_eq!(selections[&task.id], request, "each instance selected once");
                }
            }
        },
    );
}

#[test]
fn instance_projection_preserves_status_order_cursor_and_scheduler_dedupe() {
    isolated(
        "auto_tasks::instance_projection_preserves_status_order_cursor_and_scheduler_dedupe",
        || {
            let fixture = Fixture::new();
            let statuses = [
                TaskStatus::Proposed,
                TaskStatus::Backlog,
                TaskStatus::InProgress,
                TaskStatus::Review,
                TaskStatus::Blocked,
                TaskStatus::Done,
                TaskStatus::Archived,
                TaskStatus::Rejected,
                TaskStatus::Someday,
            ];
            let mut expected = BTreeMap::new();
            for status in statuses {
                let name = format!("status-{status}");
                definition(&fixture, &name);
                let task = instance(&fixture, vec![auto_task_tag(&name)], status);
                let open = matches!(
                    status,
                    TaskStatus::Proposed
                        | TaskStatus::Backlog
                        | TaskStatus::InProgress
                        | TaskStatus::Review
                        | TaskStatus::Blocked
                );
                expected.insert(name, (Some(task.id), Some(status), open));
            }
            definition(&fixture, "mixed");
            instance(&fixture, vec![auto_task_tag("mixed")], TaskStatus::Backlog);
            let newest = instance(&fixture, vec![auto_task_tag("mixed")], TaskStatus::Done);
            expected.insert(
                "mixed".into(),
                (Some(newest.id), Some(TaskStatus::Done), true),
            );
            // A task bearing two definition tags belongs to both groups.
            definition(&fixture, "shared-a");
            definition(&fixture, "shared-b");
            let shared = instance(
                &fixture,
                vec![auto_task_tag("shared-a"), auto_task_tag("shared-b")],
                TaskStatus::Someday,
            );
            for name in ["shared-a", "shared-b"] {
                expected.insert(
                    name.into(),
                    (Some(shared.id.clone()), Some(TaskStatus::Someday), false),
                );
            }
            definition(&fixture, "empty");
            definition(&fixture, "cursor-only");
            let cursor_id = newest_cursor_id(&fixture);
            let now = Utc::now();
            let baseline = now - Duration::minutes(120);
            let mut cursors = serde_json::Map::new();
            for name in expected
                .keys()
                .chain([&"empty".into(), &"cursor-only".into()])
            {
                cursors.insert(name.clone(), json!({"baseline_at":baseline}));
            }
            cursors.get_mut("cursor-only").unwrap()["last_task_id"] = json!(cursor_id);
            write_json(
                &orbit_core::application::auto_tasks::cursor_state_path(
                    &fixture.runtime.paths().state_dir,
                ),
                json!({"definitions":cursors}),
            );
            expected.insert("empty".into(), (None, None, false));
            expected.insert("cursor-only".into(), (Some(cursor_id), None, false));
            let server = fixture.server(false);
            let response = json_ok(server.get(URL));
            for row in response["definitions"].as_array().unwrap() {
                let name = row["name"].as_str().unwrap();
                let (id, status, open) = &expected[name];
                assert_eq!(row["last_minted_task_id"], json!(id), "{name}");
                assert_eq!(row["last_minted_task_status"], json!(status), "{name}");
                assert_eq!(row["open_duplicate"], json!(open), "{name}");
                assert_eq!(row["may_create_open_duplicate"], json!(open), "{name}");
                assert_eq!(
                    fixture
                        .runtime
                        .open_auto_task_instance(
                            &fixture.runtime.auto_task_show(name).unwrap().unwrap()
                        )
                        .unwrap()
                        .is_some(),
                    *open,
                    "dashboard and scheduler must agree"
                );
            }
            assert_eq!(
                response["definitions"].as_array().unwrap().len(),
                expected.len()
            );
            let outcome =
                run_auto_task_scheduler_at(&fixture.runtime, now, SchedulerOptions::default())
                    .unwrap();
            for report in outcome.reports {
                let open = expected[&report.name].2;
                assert_eq!(
                    report.action.to_string(),
                    if open { "skipped" } else { "fired" },
                    "{}: {report:?}",
                    report.name
                );
                assert_eq!(report.reason.as_deref(), open.then_some("dedupe_open"));
            }
        },
    );
}

fn newest_cursor_id(fixture: &Fixture) -> String {
    instance(fixture, vec!["cursor-reference".into()], TaskStatus::Done).id
}

/// Run the same HTTP fixture on the baseline and candidate; timings are evidence,
/// not a load-sensitive CI assertion. Seed/setup time is outside the measurement.
#[test]
#[ignore = "manual before/after latency evidence on a 4,000-task fixture"]
fn list_latency_4000_tasks() {
    isolated("auto_tasks::list_latency_4000_tasks", || {
        let fixture = Fixture::new();
        seed(&fixture, 4000);
        let server = fixture.server(false);
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            let response = json_ok(
                server
                    .request("GET", URL)
                    .timeout(std::time::Duration::from_secs(60))
                    .send()
                    .unwrap(),
            );
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(response["definitions"].as_array().unwrap().len(), 16);
            samples.push(elapsed);
        }
        let cold = samples.remove(0);
        samples.sort_by(f64::total_cmp);
        if let Some(path) = std::env::var_os("ORBIT_HTTP_LATENCY_OUTPUT") {
            write_json(
                std::path::Path::new(&path),
                json!({
                    "tasks":4000, "definitions":16, "instances":320,
                "interval_definitions":8, "delivery_definitions":8,
                    "cold_ms":cold, "warm_median_ms":(samples[2]+samples[3])/2.0,
                    "warm_samples_ms":samples,
                }),
            );
        }
    });
}
