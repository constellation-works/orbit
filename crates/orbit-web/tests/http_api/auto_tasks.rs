use std::collections::BTreeMap;
use std::fs;
use std::time::Instant;

use chrono::{Duration, Utc};
use orbit_common::test_env::FixtureProgress;
use orbit_core::application::auto_tasks::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{AuditEventStatus, AutoTaskAddParams, Task};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, auto_task_tag};
use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, json_ok, write_json};

const URL: &str = "/api/auto-tasks?workspace=ws_http_fixture";

fn definition(fixture: &Fixture, name: &str) {
    seed_auto_task_with_schedule(fixture, name, AutoTaskSchedule::every_minutes(60));
}

pub(super) fn seed_auto_task_with_schedule(
    fixture: &Fixture,
    name: &str,
    schedule: AutoTaskSchedule,
) {
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

fn seed(
    fixture: &Fixture,
    total: usize,
    progress: &mut FixtureProgress,
) -> BTreeMap<String, Vec<Task>> {
    progress.phase("seed tasks", total);
    let mut instances = BTreeMap::new();
    for index in 0..16 {
        let name = format!("fixture-{index:02}");
        let schedule = if index % 2 == 0 {
            AutoTaskSchedule::every_minutes(60)
        } else {
            serde_json::from_value(json!({"deliveries_landed": {
                "branch":"agent-main", "threshold":1, "max_wait_minutes":10,
                "coverage":"landed_code_review_v1", "max_items":50, "retries":0,
            }}))
            .unwrap()
        };
        seed_auto_task_with_schedule(fixture, &name, schedule);
        let tasks = (0..20)
            .map(|_| {
                let task = instance(fixture, vec![auto_task_tag(&name)], TaskStatus::Backlog);
                progress.advance();
                task
            })
            .collect();
        instances.insert(name, tasks);
    }
    for _ in 320..total {
        instance(fixture, vec!["unrelated".into()], TaskStatus::Done);
        progress.advance();
    }
    instances
}

#[test]
fn list_uses_one_freshness_pass_and_selects_each_instance_once() {
    isolated(
        "auto_tasks::list_uses_one_freshness_pass_and_selects_each_instance_once",
        || {
            // Seeding and two cold list requests can outlast the child's hang
            // guard on a saturated host; name the phase that was running.
            let mut progress = FixtureProgress::start("auto-task list freshness");
            let fixture = Fixture::new();
            // 40 tasks no definition minted: the listing must not read them.
            let instances = seed(&fixture, 360, &mut progress);
            progress.phase("start server", 1);
            let server = fixture.counted_task_server();
            progress.advance();
            progress.phase("list requests", 2);
            for request in 1..=2 {
                let response = json_ok(server.get(URL));
                progress.advance();
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
                assert_eq!(
                    selections.len(),
                    320,
                    "only auto-task instances are read; selecting every workspace task \
                     made the listing scale with the workspace [ORB-21824]"
                );
                for task in instances.values().flatten() {
                    assert_eq!(selections[&task.id], request, "each instance selected once");
                }
            }
            progress.finish();
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
            assert_eq!(
                outcome
                    .reports
                    .iter()
                    .map(|report| report.name.as_str())
                    .collect::<std::collections::BTreeSet<_>>(),
                expected.keys().map(String::as_str).collect(),
                "scheduler must report every expected definition"
            );
            assert_eq!(
                outcome.reports.len(),
                expected.len(),
                "one report per definition"
            );
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

/// The panel reads the listing every surface shares: the effective enabled
/// state, and the ownership skip, which no longer looks like a live delivery
/// trigger.
#[test]
fn list_reports_effective_enabled_and_the_ownership_skip() {
    isolated(
        "auto_tasks::list_reports_effective_enabled_and_the_ownership_skip",
        || {
            let fixture = Fixture::new();
            for (name, owner) in [
                ("delivery-code-review", None),
                ("elsewhere", Some("hm_elsewhere")),
            ] {
                let schedule = serde_json::from_value(json!({"deliveries_landed": {
                    "owner_machine": owner, "branch": "agent-main", "threshold": 1,
                    "max_wait_minutes": 10, "coverage": "landed_code_review_v1",
                    "max_items": 50, "retries": 0,
                }}))
                .unwrap();
                seed_auto_task_with_schedule(&fixture, name, schedule);
            }
            fixture.runtime.auto_task_toggle("elsewhere", true).unwrap();
            fixture
                .runtime
                .auto_task_toggle("delivery-code-review", true)
                .unwrap();

            let server = fixture.server(false);
            let listed = json_ok(server.get(URL));
            let rows = listed["definitions"].as_array().unwrap();
            let row = |name: &str| rows.iter().find(|row| row["name"] == name).unwrap();

            let consumer = row("delivery-code-review");
            assert_eq!(consumer["enabled"], true, "{consumer}");
            assert_eq!(consumer["effective_enabled"], true, "{consumer}");
            assert_ne!(
                consumer["next_evaluation"]["state"], "disabled",
                "the schedule state reads the effective value: {consumer}"
            );
            assert!(consumer["skipped_reason"].is_null(), "{consumer}");

            let elsewhere = row("elsewhere");
            assert_eq!(elsewhere["effective_enabled"], true, "{elsewhere}");
            assert_eq!(elsewhere["plugin_inactive"], false, "{elsewhere}");
            assert!(
                elsewhere["skipped_reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("hm_elsewhere")),
                "{elsewhere}"
            );
            assert_eq!(
                elsewhere["next_evaluation"]["state"], "unavailable",
                "{elsewhere}"
            );
            assert!(elsewhere["next_evaluation"]["at"].is_null(), "{elsewhere}");
        },
    );
}

fn deliveries(owner: Option<&str>) -> AutoTaskSchedule {
    serde_json::from_value(json!({"deliveries_landed": {
        "owner_machine": owner, "branch": "agent-main", "threshold": 1,
        "max_wait_minutes": 10, "coverage": "landed_code_review_v1",
        "max_items": 50, "retries": 0,
    }}))
    .unwrap()
}

/// Time cursor failures affect only time schedules. Delivery inspection still
/// fails closed on its own state, with disabled and plugin skip precedence.
#[test]
fn list_keeps_delivery_schedules_independent_of_corrupt_time_cursors() {
    isolated(
        "auto_tasks::list_keeps_delivery_schedules_independent_of_corrupt_time_cursors",
        || {
            let fixture = Fixture::new();
            for (name, owner, enabled) in [
                ("delivery", None, true),
                ("elsewhere", Some("hm_elsewhere"), true),
                ("disabled", Some("hm_elsewhere"), false),
                ("inactive", Some("hm_elsewhere"), true),
            ] {
                seed_auto_task_with_schedule(&fixture, name, deliveries(owner));
                fixture.runtime.auto_task_toggle(name, enabled).unwrap();
            }
            definition(&fixture, "interval");
            seed_auto_task_with_schedule(&fixture, "cron", AutoTaskSchedule::cron("* * * * *"));
            // The missing seeding plugin makes the enabled definition inactive.
            let inactive = fixture.work.join("auto_tasks/inactive.yaml");
            let body = fs::read_to_string(&inactive).unwrap();
            fs::write(
                &inactive,
                format!("# provenance: plugin:fixture-missing@1.0.0\n{body}"),
            )
            .unwrap();
            let cursor = orbit_core::application::auto_tasks::cursor_state_path(
                &fixture.runtime.paths().state_dir,
            );
            fs::create_dir_all(cursor.parent().unwrap()).unwrap();
            fs::write(&cursor, "{broken").unwrap();

            let server = fixture.server(false);
            let listed = json_ok(server.get(URL));
            assert_eq!(listed["inactive_plugin_count"], 1, "{listed}");
            assert!(
                listed["definitions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["name"] != "inactive")
            );
            let url = format!("{URL}&include_inactive_plugins=true");
            let listed = json_ok(server.get(&url));
            let row = |listed: &Value, name: &str| {
                listed["definitions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["name"] == name)
                    .unwrap()
                    .clone()
            };
            assert!(listed["cursor_state_error"].is_string(), "{listed}");
            for (name, state) in [
                ("delivery", "waiting"),
                ("interval", "unavailable"),
                ("cron", "unavailable"),
                ("elsewhere", "unavailable"),
                ("disabled", "disabled"),
                ("inactive", "waiting"),
            ] {
                let item = row(&listed, name);
                assert_eq!(item["next_evaluation"]["state"], state, "{item}");
                assert!(item["next_evaluation"]["at"].is_null(), "{item}");
            }
            let delivery = row(&listed, "delivery");
            assert!(delivery["skipped_reason"].is_null(), "{delivery}");
            assert_ne!(
                delivery["automation"]["reason"], "state_unavailable",
                "{delivery}"
            );
            let inactive = row(&listed, "inactive");
            assert_eq!(inactive["plugin_inactive"], true, "{inactive}");
            assert!(
                inactive["skipped_reason"]
                    .as_str()
                    .unwrap()
                    .contains("fixture-missing"),
                "{inactive}"
            );

            // Malformed trigger state is independent of malformed time cursors.
            for name in ["delivery", "disabled"] {
                let consumer = orbit_core::application::automation::consumer_key(
                    &fixture.runtime,
                    "auto-task",
                    name,
                )
                .unwrap();
                fixture.runtime.sqlite_store().unwrap().with_transaction(|tx| {
                    tx.connection().execute(
                        "INSERT INTO automation_consumers (consumer, generation, state_json) VALUES (?1, 1, ?2)",
                        [consumer.as_str(), "{broken-trigger"],
                    ).map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                    Ok(())
                }).unwrap();
            }
            let listed = json_ok(server.get(&url));
            assert!(listed["cursor_state_error"].is_string(), "{listed}");
            let delivery = row(&listed, "delivery");
            assert_eq!(
                delivery["automation"]["reason"], "state_unavailable",
                "{delivery}"
            );
            assert_eq!(
                delivery["next_evaluation"]["state"], "unavailable",
                "{delivery}"
            );
            assert_eq!(
                row(&listed, "disabled")["next_evaluation"]["state"],
                "disabled"
            );
            assert_eq!(
                fs::read_to_string(&cursor).unwrap(),
                "{broken",
                "listing must preserve corrupt cursor evidence"
            );
        },
    );
}

/// Disable on an already-disabled consumer is a no-op: the raw flag is
/// already false, so the effective switch already matches. An ordinary
/// raw-off definition stays a no-op, and a stale raw token is still a conflict.
#[test]
fn disable_disabled_consumer_is_a_no_op_and_keeps_raw_cas() {
    isolated(
        "auto_tasks::disable_disabled_consumer_is_a_no_op_and_keeps_raw_cas",
        || {
            let fixture = Fixture::new();
            seed_auto_task_with_schedule(&fixture, "delivery-code-review", deliveries(None));
            seed_auto_task_with_schedule(&fixture, "parked", deliveries(None));
            definition(&fixture, "nightly");

            let server = fixture.server(true);
            let toggle = "/api/auto-tasks/toggle?workspace=ws_http_fixture";
            let row = |listed: &Value, name: &str| {
                listed["definitions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["name"] == name)
                    .unwrap()
                    .clone()
            };
            let listed = json_ok(server.get(URL));
            let consumer = row(&listed, "delivery-code-review");
            assert_eq!(consumer["enabled"], false, "{consumer}");
            assert_eq!(consumer["effective_enabled"], false, "{consumer}");
            let parked = row(&listed, "parked");
            assert_eq!(parked["enabled"], false, "{parked}");
            assert_eq!(parked["effective_enabled"], false, "{parked}");
            let nightly = row(&listed, "nightly");
            assert_eq!(nightly["enabled"], true, "{nightly}");
            assert_eq!(nightly["effective_enabled"], true, "{nightly}");

            let unchanged = json_ok(server.send(
                "POST",
                toggle,
                json!({"name":"parked","expected_enabled":false,"enabled":false}),
            ));
            assert_eq!(unchanged["changed"], false, "{unchanged}");
            let listed = json_ok(server.get(URL));
            assert_eq!(row(&listed, "parked")["effective_enabled"], false);

            let conflict = server.send(
                "POST",
                toggle,
                json!({"name":"nightly","expected_enabled":false,"enabled":false}),
            );
            assert_eq!(conflict.status().as_u16(), 409);
            let conflict: Value = conflict.json().unwrap();
            assert_eq!(conflict["code"], "stale_auto_task_state");
            assert_eq!(conflict["actual_enabled"], true, "{conflict}");
            let listed = json_ok(server.get(URL));
            assert_eq!(row(&listed, "nightly")["enabled"], true, "{listed}");

            let disabled = json_ok(server.send(
                "POST",
                toggle,
                json!({"name":"delivery-code-review","expected_enabled":false,"enabled":false}),
            ));
            assert_eq!(disabled["changed"], false, "{disabled}");
            assert_eq!(disabled["enabled"], false, "{disabled}");
            let listed = json_ok(server.get(URL));
            let consumer = row(&listed, "delivery-code-review");
            assert_eq!(consumer["enabled"], false, "{consumer}");
            assert_eq!(consumer["effective_enabled"], false, "{consumer}");

            let second = json_ok(server.send(
                "POST",
                toggle,
                json!({"name":"delivery-code-review","expected_enabled":false,"enabled":false}),
            ));
            assert_eq!(second["changed"], false, "{second}");

            let ordinary = json_ok(server.send(
                "POST",
                toggle,
                json!({"name":"nightly","expected_enabled":true,"enabled":false}),
            ));
            assert_eq!(ordinary["changed"], true, "{ordinary}");
            let listed = json_ok(server.get(URL));
            let nightly = row(&listed, "nightly");
            assert_eq!(nightly["enabled"], false, "{nightly}");
            assert_eq!(nightly["effective_enabled"], false, "{nightly}");
        },
    );
}

fn newest_cursor_id(fixture: &Fixture) -> String {
    instance(fixture, vec!["cursor-reference".into()], TaskStatus::Done).id
}

const DELETE_URL: &str = "/api/auto-tasks/delete?workspace=ws_http_fixture";

fn listed_names(server: &super::support::Server) -> Vec<String> {
    json_ok(server.get(URL))["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_string())
        .collect()
}

fn delete_audits(fixture: &Fixture, status: AuditEventStatus) -> Vec<orbit_core::AuditEvent> {
    fixture
        .runtime
        .list_audit_events_with_kind(
            None,
            None,
            Some("auto_task.delete".into()),
            Some(status),
            None,
            20,
        )
        .unwrap()
}

/// Authorized delete, the open minted-task refusal, and an operator denial,
/// each with its dashboard audit record.
#[test]
fn delete_audits_authorized_removal_open_task_refusal_and_operator_denial() {
    isolated(
        "auto_tasks::delete_audits_authorized_removal_open_task_refusal_and_operator_denial",
        || {
            let fixture = Fixture::new();
            definition(&fixture, "retire-me");
            definition(&fixture, "held-open");
            let open = instance(
                &fixture,
                vec![auto_task_tag("held-open")],
                TaskStatus::Backlog,
            );

            {
                let agent = fixture.server(false);
                let denied = error_code(
                    agent.send(
                        "POST",
                        DELETE_URL,
                        json!({"name":"retire-me","reason":"nope"}),
                    ),
                    403,
                    "authorization_denied",
                );
                assert_eq!(denied["operation"], json!("auto_task.delete"), "{denied}");
                assert!(
                    fixture
                        .runtime
                        .auto_task_show("retire-me")
                        .unwrap()
                        .is_some(),
                    "a denied delete must leave the definition"
                );
                assert!(listed_names(&agent).iter().any(|name| name == "retire-me"));
            }
            let denied = delete_audits(&fixture, AuditEventStatus::Denied);
            assert_eq!(denied.len(), 1, "{denied:?}");
            assert_eq!(denied[0].target_id.as_deref(), Some("retire-me"));
            assert_eq!(denied[0].command, "dashboard.operations");
            assert!(denied[0].error_message.is_some(), "{denied:?}");

            let operator = fixture.server(true);
            let refused = error_code(
                operator.send(
                    "POST",
                    DELETE_URL,
                    json!({"name":"held-open","reason":"cleanup"}),
                ),
                409,
                "open_minted_tasks",
            );
            assert!(
                refused["open_tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id.as_str() == Some(open.id.as_str())),
                "{refused}"
            );
            assert!(
                refused["error"]
                    .as_str()
                    .unwrap()
                    .contains(open.id.as_str()),
                "{refused}"
            );
            assert!(
                fixture
                    .runtime
                    .auto_task_show("held-open")
                    .unwrap()
                    .is_some()
            );
            assert!(
                listed_names(&operator)
                    .iter()
                    .any(|name| name == "held-open")
            );
            let failures = delete_audits(&fixture, AuditEventStatus::Failure);
            assert_eq!(failures.len(), 1, "{failures:?}");
            assert_eq!(failures[0].target_id.as_deref(), Some("held-open"));
            assert!(
                failures[0]
                    .error_message
                    .as_deref()
                    .unwrap_or("")
                    .contains(open.id.as_str()),
                "{failures:?}"
            );

            let forced = json_ok(operator.send(
                "POST",
                DELETE_URL,
                json!({"name":"held-open","reason":"cleanup","force":true}),
            ));
            assert_eq!(forced["deleted"], json!(true), "{forced}");
            assert_eq!(forced["name"], json!("held-open"), "{forced}");
            assert!(forced.get("path").is_none(), "{forced}");
            assert!(
                forced["open_tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id.as_str() == Some(open.id.as_str())),
                "{forced}"
            );
            assert!(
                fixture
                    .runtime
                    .auto_task_show("held-open")
                    .unwrap()
                    .is_none()
            );

            let deleted = json_ok(operator.send(
                "POST",
                DELETE_URL,
                json!({"name":"retire-me","reason":"retired"}),
            ));
            assert_eq!(deleted["deleted"], json!(true), "{deleted}");
            assert_eq!(deleted["opted_out"], json!(false), "{deleted}");
            assert_eq!(deleted["reason"], json!("retired"), "{deleted}");
            assert!(deleted.get("path").is_none(), "{deleted}");
            assert!(
                deleted["message"]
                    .as_str()
                    .unwrap()
                    .contains("Deleted auto-task retire-me"),
                "{deleted}"
            );
            assert!(
                fixture
                    .runtime
                    .auto_task_show("retire-me")
                    .unwrap()
                    .is_none()
            );
            let names = listed_names(&operator);
            assert!(!names.iter().any(|name| name == "retire-me"), "{names:?}");
            assert!(!names.iter().any(|name| name == "held-open"), "{names:?}");

            let successes = delete_audits(&fixture, AuditEventStatus::Success);
            assert_eq!(successes.len(), 2, "{successes:?}");
            let retired = successes
                .iter()
                .find(|event| event.target_id.as_deref() == Some("retire-me"))
                .expect("success audit for retire-me");
            let arguments = retired.arguments_json.as_deref().unwrap_or("");
            assert!(arguments.contains("retired"), "{arguments}");
            assert_eq!(retired.command, "dashboard.operations");
        },
    );
}

/// Run the same HTTP fixture on the baseline and candidate; timings are evidence,
/// not a load-sensitive CI assertion. Seed/setup time is outside the measurement.
#[test]
#[ignore = "manual before/after latency evidence on a 4,000-task fixture"]
fn list_latency_4000_tasks() {
    isolated("auto_tasks::list_latency_4000_tasks", || {
        let mut progress = FixtureProgress::start("auto-task list latency");
        let fixture = Fixture::new();
        seed(&fixture, 4000, &mut progress);
        progress.finish();
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

/// ORB-21174 preserves the HTTP row contract while moving its projection to Core.
#[test]
fn listing_profile_preserves_http_fields_and_condition_presence() {
    isolated(
        "auto_tasks::listing_profile_preserves_http_fields_and_condition_presence",
        || {
            let fixture = Fixture::new();
            let secret = format!("ghp_{}", "a".repeat(36));
            let raw = format!("  {secret} {}  ", "π".repeat(2200));
            let mut definitions = Vec::new();
            for (name, schedule, dedupe) in [
                (
                    "profile-interval",
                    AutoTaskSchedule::every_minutes(60),
                    DedupePolicy::Always,
                ),
                (
                    "profile-condition",
                    serde_json::from_value(json!({"condition":{"scope":"task","when":"'profile-fixture' in task.tags"}}))
                        .unwrap(),
                    DedupePolicy::SkipIfOpen,
                ),
            ] {
                definitions.push(
                    fixture
                        .runtime
                        .auto_task_add(AutoTaskAddParams {
                            name: name.into(),
                            description: raw.clone(),
                            schedule,
                            template: AutoTaskTemplate {
                                title: raw.clone(),
                                description: "Omitted HTTP body".into(),
                                acceptance_criteria: vec!["Profile preserved".into()],
                                task_type: TaskType::Chore,
                                tags: vec!["profile".into()],
                                required_tools: vec![],
                                context_files: vec![],
                                priority: TaskPriority::High,
                                complexity: Some(orbit_types::task::TaskComplexity::Low),
                                crew: None,
                                status: TaskStatus::Proposed,
                            },
                            dedupe,
                        })
                        .unwrap(),
                );
            }
            let server = fixture.server(true);
            let page = json_ok(server.get(URL));
            for definition in definitions {
                let row = page["definitions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["name"] == definition.name)
                    .unwrap();
                let mut keys = row
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>();
                keys.sort_unstable();
                assert_eq!(
                    keys,
                    vec![
                        "automation",
                        "condition",
                        "dedupe",
                        "description",
                        "effective_enabled",
                        "enabled",
                        "inactive_plugin",
                        "last_evaluation",
                        "last_minted_task_id",
                        "last_minted_task_status",
                        "may_create_open_duplicate",
                        "name",
                        "next_evaluation",
                        "open_duplicate",
                        "plugin_inactive",
                        "schedule",
                        "schedule_summary",
                        "skipped_reason",
                        "template",
                        "template_summary"
                    ]
                );
                assert_eq!(row["description"], raw);
                assert_eq!(row["enabled"], definition.enabled);
                assert_eq!(row["effective_enabled"], definition.enabled);
                assert_eq!(row["schedule"], json!(definition.schedule));
                assert_eq!(
                    row["template"],
                    json!({"title":raw,"crew":null,"status":"proposed","priority":"high","complexity":"low","required_tools":[]})
                );
                assert_eq!(
                    row["template_summary"],
                    format!("[auto-task] {raw} · status proposed · priority high · complexity low")
                );
                assert_eq!(row["plugin_inactive"], false);
                assert_eq!(row["open_duplicate"], false);
                assert_eq!(row["may_create_open_duplicate"], false);
                for field in [
                    "automation",
                    "inactive_plugin",
                    "skipped_reason",
                    "last_evaluation",
                    "last_minted_task_id",
                    "last_minted_task_status",
                ] {
                    assert!(row[field].is_null(), "{row}");
                }
                if definition.name == "profile-condition" {
                    assert!(row["condition"].is_object(), "{row}");
                    assert_eq!(row["dedupe"], "skip_if_open");
                    assert_eq!(
                        row["schedule_summary"],
                        "task condition: 'profile-fixture' in task.tags"
                    );
                } else {
                    assert!(row["condition"].is_null(), "{row}");
                    assert_eq!(row["dedupe"], "always");
                    assert_eq!(row["schedule_summary"], "every 60 minutes");
                }
            }
        },
    );
}
