use std::fs;

use chrono::Utc;
use orbit_core::application::auto_tasks::{SchedulerOptions, run_auto_task_scheduler_at};
use orbit_types::workflow::AutoTaskSchedule;
use serde_json::{Value, json};

use super::auto_tasks;
use super::support::{Fixture, isolated, json_ok};

/// The dashboard's whole-value timing edits keep guards in durable definitions;
/// stale editors cannot overwrite them. The browser fixture supplies these same
/// six carrier shapes through the real Operations editors.
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
