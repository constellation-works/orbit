use std::fs;

use chrono::Utc;
use orbit_core::AuditEventStatus;
use orbit_core::application::auto_tasks::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_types::workflow::AutoTaskSchedule;
use serde_json::{Value, json};

use super::auto_tasks;
use super::support::{Fixture, error_code, isolated, json_ok};

/// The dashboard's whole-value timing edits keep guards in durable definitions;
/// stale editors cannot overwrite them. These six carrier shapes are posted
/// straight to the API; no automated test drives the Operations editors.
#[test]
fn dashboard_timing_edits_persist_guards_and_refuse_stale_values() {
    isolated(
        "schedule_edit_guards::dashboard_timing_edits_persist_guards_and_refuse_stale_values",
        || {
            let fixture = Fixture::new();
            fixture.job("task_pilot_pipeline");
            let owner = fixture.runtime.automation_machine_identity().unwrap();
            let delivery = json!({
                "branch":"agent-main", "threshold":2, "max_wait_minutes":10,
                "max_items":5, "retries":0, "coverage":"landed_code_review_v1",
            });
            let state = json!({
                "kind":"preparation_eligible", "owner_machine":owner,
                "branch":"agent-main", "debounce_minutes":2, "max_wait_minutes":10,
                "max_items":5, "retries":0, "deadline_minutes":30,
            });
            let mut edited_delivery = delivery.clone();
            edited_delivery["threshold"] = json!(3);
            let mut edited_state = state.clone();
            edited_state["debounce_minutes"] = json!(3);
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            let server = fixture.server(true);
            for (kind, before, after) in [
                (
                    "cron",
                    json!({"cron":"* * * * *"}),
                    json!({"cron":"*/2 * * * *"}),
                ),
                (
                    "interval",
                    json!({"every_minutes":15}),
                    json!({"every_minutes":30}),
                ),
                (
                    "delivery",
                    json!({"deliveries_landed":delivery}),
                    json!({"deliveries_landed":edited_delivery}),
                ),
                (
                    "state",
                    json!({"state":state}),
                    json!({"state":edited_state}),
                ),
            ] {
                for routine in [false, true] {
                    if (routine && kind == "interval") || (!routine && kind == "state") {
                        continue;
                    }
                    let name = format!("guarded-{kind}");
                    let mut expected = before.clone();
                    let mut edited = after.clone();
                    for value in [&mut expected, &mut edited] {
                        value["when"] = json!("  false  ");
                        if kind == "cron" || kind == "interval" {
                            value["guard_miss"] = json!("skip");
                        }
                        if routine {
                            value["missed_run"] = json!("skip");
                        }
                    }
                    let (endpoint, body) = if routine {
                        fs::write(routines.join(format!("{name}.yaml")), format!(
                            "schemaVersion: 1\nname: {name}\nenabled: true\ntarget: job:task_pilot_pipeline\ntrigger: {expected}\n"
                        )).unwrap();
                        let report = json_ok(server.get("/api/routines?workspace=ws_http_fixture"));
                        let row = report["routines"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|row| row["name"] == name)
                            .unwrap();
                        assert_eq!(row["trigger"]["when"], expected["when"]);
                        (
                            "/api/routines/trigger?workspace=ws_http_fixture",
                            json!({
                                "name":name, "source":row["source"], "target":row["target"],
                                "machine_name":"fixture", "expected_trigger":row["trigger"], "trigger":edited,
                            }),
                        )
                    } else {
                        auto_tasks::seed_auto_task_with_schedule(
                            &fixture,
                            &name,
                            serde_json::from_value(expected.clone()).unwrap(),
                        );
                        (
                            "/api/auto-tasks/schedule?workspace=ws_http_fixture",
                            json!({
                                "name":name, "expected_schedule":expected, "schedule":edited,
                            }),
                        )
                    };
                    json_ok(server.send("POST", endpoint, body.clone()));
                    let read = || {
                        if routine {
                            let report =
                                json_ok(server.get("/api/routines?workspace=ws_http_fixture"));
                            report["routines"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .find(|row| row["name"] == name)
                                .unwrap()["trigger"]
                                .clone()
                        } else {
                            serde_json::to_value(
                                fixture
                                    .runtime
                                    .auto_task_show(&name)
                                    .unwrap()
                                    .unwrap()
                                    .schedule,
                            )
                            .unwrap()
                        }
                    };
                    let persisted = read();
                    // Compare the typed wire form, including default fields
                    // emitted for delivery/state carriers.
                    let canonical = if routine {
                        serde_json::to_value(
                            serde_json::from_value::<orbit_types::workflow::RoutineTrigger>(
                                edited.clone(),
                            )
                            .unwrap(),
                        )
                        .unwrap()
                    } else {
                        serde_json::to_value(
                            serde_json::from_value::<AutoTaskSchedule>(edited.clone()).unwrap(),
                        )
                        .unwrap()
                    };
                    assert_eq!(persisted, canonical, "{kind}, routine={routine}");
                    let stale = server.send("POST", endpoint, body);
                    assert_eq!(stale.status().as_u16(), 409, "{kind}, routine={routine}");
                    assert_eq!(
                        stale.json::<Value>().unwrap()["code"],
                        if routine {
                            "stale_routine_trigger"
                        } else {
                            "stale_auto_task_schedule"
                        }
                    );
                    assert_eq!(
                        read(),
                        persisted,
                        "stale edit must not change the durable guard"
                    );
                }
            }
            // The interval and cron are due here. Their saved false guards
            // must still suppress minting after a timing edit.
            let now = chrono::DateTime::parse_from_rfc3339("2026-10-31T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc);
            run_auto_task_scheduler_at(&fixture.runtime, now, SchedulerOptions::default()).unwrap();
            let outcome = run_auto_task_scheduler_at(
                &fixture.runtime,
                now + chrono::Duration::minutes(30),
                SchedulerOptions::default(),
            )
            .unwrap();
            for name in ["guarded-cron", "guarded-interval"] {
                let report = outcome
                    .reports
                    .iter()
                    .find(|report| report.name == name)
                    .unwrap();
                assert_eq!(report.action.to_string(), "skipped", "{report:?}");
                assert!(
                    report
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.starts_with("guard_false:")),
                    "{report:?}"
                );
                assert!(
                    report.task_id.is_none(),
                    "a closed guard must not mint: {report:?}"
                );
            }
        },
    );
}

fn denied_schedule_audits(fixture: &Fixture, operation: &str) -> Vec<orbit_core::AuditEvent> {
    fixture
        .runtime
        .list_audit_events_with_kind(
            None,
            None,
            Some(operation.into()),
            Some(AuditEventStatus::Denied),
            None,
            20,
        )
        .unwrap()
}

fn arguments_of(event: &orbit_core::AuditEvent) -> Value {
    serde_json::from_str(event.arguments_json.as_deref().unwrap_or("{}")).unwrap()
}

/// A mistyped nested trigger or schedule must still leave a denied audit for
/// a non-operator, and an `invalid_schedule` refusal for an operator, without
/// writing the definition.
#[test]
fn invalid_nested_timing_edits_audit_denials_and_refuse_operators() {
    isolated(
        "schedule_edit_guards::invalid_nested_timing_edits_audit_denials_and_refuse_operators",
        || {
            let fixture = Fixture::new();
            fixture.job("task_pilot_pipeline");
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            fs::write(
                routines.join("denied-routine.yaml"),
                "schemaVersion: 1\nname: denied-routine\nenabled: true\ntarget: job:task_pilot_pipeline\ntrigger: {\"cron\":\"* * * * *\",\"missed_run\":\"skip\"}\n",
            )
            .unwrap();
            auto_tasks::seed_auto_task_with_schedule(
                &fixture,
                "denied-auto",
                AutoTaskSchedule::cron("* * * * *"),
            );
            let stored_schedule = serde_json::to_value(
                fixture
                    .runtime
                    .auto_task_show("denied-auto")
                    .unwrap()
                    .unwrap()
                    .schedule,
            )
            .unwrap();
            let agent = fixture.server(false);
            let operator = fixture.server(true);
            let report = json_ok(operator.get("/api/routines?workspace=ws_http_fixture"));
            let row = report["routines"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == "denied-routine")
                .unwrap();
            let stored_trigger = row["trigger"].clone();
            let routine_url = "/api/routines/trigger?workspace=ws_http_fixture";
            let schedule_url = "/api/auto-tasks/schedule?workspace=ws_http_fixture";

            let mut bad_trigger = stored_trigger.clone();
            bad_trigger["cron"] = json!(15);
            let mut bad_schedule = stored_schedule.clone();
            bad_schedule["cron"] = json!(15);
            let source = row["source"].clone();
            let target = row["target"].clone();

            let cases = [
                (
                    "routine.set_trigger",
                    routine_url,
                    "denied-routine",
                    json!({
                        "name": "denied-routine",
                        "source": source.clone(),
                        "target": target.clone(),
                        "machine_name": "fixture",
                        "expected_trigger": bad_trigger.clone(),
                        "trigger": stored_trigger.clone(),
                    }),
                    "trigger",
                    stored_trigger.clone(),
                ),
                (
                    "routine.set_trigger",
                    routine_url,
                    "denied-routine",
                    json!({
                        "name": "denied-routine",
                        "source": source.clone(),
                        "target": target.clone(),
                        "machine_name": "fixture",
                        "expected_trigger": stored_trigger.clone(),
                        "trigger": bad_trigger.clone(),
                    }),
                    "trigger",
                    bad_trigger.clone(),
                ),
                (
                    "auto_task.set_schedule",
                    schedule_url,
                    "denied-auto",
                    json!({
                        "name": "denied-auto",
                        "expected_schedule": bad_schedule.clone(),
                        "schedule": stored_schedule.clone(),
                    }),
                    "schedule",
                    stored_schedule.clone(),
                ),
                (
                    "auto_task.set_schedule",
                    schedule_url,
                    "denied-auto",
                    json!({
                        "name": "denied-auto",
                        "expected_schedule": stored_schedule.clone(),
                        "schedule": bad_schedule.clone(),
                    }),
                    "schedule",
                    bad_schedule.clone(),
                ),
            ];

            for (operation, url, name, body, argument, submitted) in &cases {
                let before = denied_schedule_audits(&fixture, operation);
                let denied = error_code(
                    agent.send("POST", url, body.clone()),
                    403,
                    "authorization_denied",
                );
                assert_eq!(denied["operation"], json!(*operation), "{denied}");
                let after = denied_schedule_audits(&fixture, operation);
                assert_eq!(after.len(), before.len() + 1, "{operation}: {after:?}");
                assert!(
                    after.iter().all(|event| {
                        event.command == "dashboard.operations"
                            && event.target_id.as_deref() == Some(name)
                            && event.workspace_id.as_deref() == Some("ws_http_fixture")
                            && event.status == AuditEventStatus::Denied
                    }),
                    "{after:?}"
                );
                assert!(
                    after
                        .iter()
                        .any(|event| arguments_of(event).get(*argument) == Some(submitted)),
                    "{operation} denied row must record the submitted {argument}: {after:?}"
                );
            }

            let routine_after = json_ok(operator.get("/api/routines?workspace=ws_http_fixture"));
            let routine_now = routine_after["routines"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == "denied-routine")
                .unwrap()["trigger"]
                .clone();
            assert_eq!(
                routine_now, stored_trigger,
                "a denied trigger edit must not write"
            );
            let schedule_now = serde_json::to_value(
                fixture
                    .runtime
                    .auto_task_show("denied-auto")
                    .unwrap()
                    .unwrap()
                    .schedule,
            )
            .unwrap();
            assert_eq!(
                schedule_now, stored_schedule,
                "a denied schedule edit must not write"
            );

            let routine_denials = denied_schedule_audits(&fixture, "routine.set_trigger").len();
            let schedule_denials = denied_schedule_audits(&fixture, "auto_task.set_schedule").len();
            for (operation, url, body) in [
                ("routine.set_trigger", routine_url, cases[0].3.clone()),
                ("routine.set_trigger", routine_url, cases[1].3.clone()),
                ("auto_task.set_schedule", schedule_url, cases[2].3.clone()),
                ("auto_task.set_schedule", schedule_url, cases[3].3.clone()),
            ] {
                let refused = error_code(operator.send("POST", url, body), 400, "invalid_schedule");
                assert!(
                    refused["error"]
                        .as_str()
                        .is_some_and(|error| error.starts_with("invalid request body:")),
                    "{refused}"
                );
                assert_eq!(
                    denied_schedule_audits(&fixture, operation).len(),
                    if operation == "routine.set_trigger" {
                        routine_denials
                    } else {
                        schedule_denials
                    },
                    "an operator refusal must not add a denied audit"
                );
            }
            let routine_after_operator =
                json_ok(operator.get("/api/routines?workspace=ws_http_fixture"));
            assert_eq!(
                routine_after_operator["routines"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["name"] == "denied-routine")
                    .unwrap()["trigger"],
                stored_trigger
            );
            assert_eq!(
                serde_json::to_value(
                    fixture
                        .runtime
                        .auto_task_show("denied-auto")
                        .unwrap()
                        .unwrap()
                        .schedule,
                )
                .unwrap(),
                stored_schedule
            );
        },
    );
}

/// A schedule edit for a missing definition is a 404, and a request the stored
/// schedule already satisfies answers `changed: false` without stamping.
#[test]
fn dashboard_schedule_edit_reports_missing_and_already_matched_without_writing() {
    isolated(
        "schedule_edit_guards::dashboard_schedule_edit_reports_missing_and_already_matched_without_writing",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let endpoint = "/api/auto-tasks/schedule?workspace=ws_http_fixture";
            let missing = server.send(
                "POST",
                endpoint,
                json!({"name":"absent","expected_schedule":{"every_minutes":15},"schedule":{"every_minutes":30}}),
            );
            assert_eq!(missing.status().as_u16(), 404);
            assert_eq!(
                missing.json::<Value>().unwrap()["code"],
                "auto_task_not_found"
            );

            auto_tasks::seed_auto_task_with_schedule(
                &fixture,
                "matched",
                AutoTaskSchedule::every_minutes(15),
            );
            let stamp = || {
                fixture
                    .runtime
                    .auto_task_show("matched")
                    .unwrap()
                    .unwrap()
                    .updated_at
            };
            let before = stamp();
            let matched = json_ok(server.send(
                "POST",
                endpoint,
                json!({"name":"matched","expected_schedule":{"every_minutes":15},"schedule":{"every_minutes":15}}),
            ));
            assert_eq!(matched["changed"], false);
            assert_eq!(
                stamp(),
                before,
                "an already-matched schedule is not stamped"
            );
        },
    );
}
