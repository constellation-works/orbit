use std::fs;

use chrono::{DateTime, Duration, Timelike, Utc};
use orbit_core::AutoTaskAddParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok, write_json};

#[test]
fn scoreboard_windows_scope_metrics_and_timestamp_arithmetic() {
    isolated(
        "projections::scoreboard_windows_scope_metrics_and_timestamp_arithmetic",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            // Multiple month partitions and a future row catch both lower/upper bound mistakes.
            for (index, age, retries) in [
                (0, Duration::minutes(10), 1),
                (1, Duration::hours(2), 2),
                (2, Duration::days(2), 4),
                (3, Duration::days(10), 8),
                (4, Duration::days(40), 16),
                (5, Duration::minutes(-10), 32),
            ] {
                let ts = now - age;
                let dir = fixture
                    .runtime
                    .data_root()
                    .join("state/diagnostics/metrics")
                    .join(ts.format("%Y-%m").to_string());
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join(format!("row-{index}.jsonl")), format!("{}\n", json!({
                "ts":ts.to_rfc3339(),"job_run":"jrun-scoreboard","step":"implement",
                "actor_identity":"http-metrics-fixture","step_duration_ms":100,"retry_count":retries,
                "tool_invocations":1,"token_usage":10,
            }))).unwrap();
            }
            let server = fixture.server(false);
            for (window, duration, expected) in [
                ("1h", Some(Duration::hours(1)), 1),
                ("24h", Some(Duration::hours(24)), 3),
                ("7d", Some(Duration::days(7)), 7),
                ("30d", Some(Duration::days(30)), 15),
                ("all", None, 31),
            ] {
                let before = Utc::now();
                let result = json_ok(server.get(&format!(
                    "/api/scoreboard?window={window}&workspace=ws_http_fixture"
                )));
                let after = Utc::now();
                assert_eq!(result["window"], window);
                assert_eq!(
                    result["agents"]["http-metrics-fixture"]["retries"], expected,
                    "scoreboard {window} must use only its own population"
                );
                if let Some(duration) = duration {
                    let since =
                        DateTime::parse_from_rfc3339(result["window_since"].as_str().unwrap())
                            .unwrap()
                            .with_timezone(&Utc);
                    assert!(
                        since >= before - duration && since <= after - duration,
                        "{window} timestamp arithmetic: {since}"
                    );
                } else {
                    assert!(result["window_since"].is_null());
                }
            }
            let default = json_ok(server.get("/api/scoreboard?workspace=ws_http_fixture"));
            assert_eq!(default["window"], "all");
            assert_eq!(
                server
                    .get("/api/scoreboard?window=bogus&workspace=ws_http_fixture")
                    .status()
                    .as_u16(),
                400
            );
        },
    );
}

#[test]
fn duration_overflow_is_a_client_error_and_server_remains_responsive() {
    isolated(
        "projections::duration_overflow_is_a_client_error_and_server_remains_responsive",
        || {
            let fixture = Fixture::new();
            fs::write(fixture.path("process.log"), "").unwrap();
            let server = fixture.server(false);
            for duration in [
                "18446744073709551616s",
                "18446744073709551615w",
                "9223372036854775808",
                "9223372036854775807",
                "999999999999999999999999d",
            ] {
                for endpoint in ["audit/summary", "log"] {
                    let response = server.get(&format!(
                        "/api/{endpoint}?since={duration}&workspace=ws_http_fixture"
                    ));
                    assert_eq!(
                        response.status().as_u16(),
                        400,
                        "overflow {duration} on {endpoint}"
                    );
                    assert!(response.json::<Value>().unwrap()["error"].is_string());
                }
            }
            assert_eq!(server.get("/healthz").status().as_u16(), 200);
            for duration in ["30m", "2h", "1d", "2w", "120"] {
                json_ok(server.get(&format!(
                    "/api/audit/summary?since={duration}&workspace=ws_http_fixture"
                )));
            }
        },
    );
}

#[test]
fn auto_task_and_routine_schedules_distinguish_armed_and_hypothetical_times() {
    isolated(
        "projections::auto_task_and_routine_schedules_distinguish_armed_and_hypothetical_times",
        || {
            let fixture = Fixture::new();
            for (name, schedule) in [
                ("interval", AutoTaskSchedule::Interval { every_minutes: 60 }),
                (
                    "cron",
                    AutoTaskSchedule::Cron {
                        cron: "* * * * *".into(),
                    },
                ),
                ("disabled", AutoTaskSchedule::Interval { every_minutes: 60 }),
                (
                    "unobserved",
                    AutoTaskSchedule::Interval { every_minutes: 60 },
                ),
            ] {
                fixture
                    .runtime
                    .auto_task_add(AutoTaskAddParams {
                        name: name.into(),
                        description: "projection fixture".into(),
                        schedule,
                        template: AutoTaskTemplate {
                            title: format!("Projection {name}"),
                            description: "HTTP schedule fixture".into(),
                            acceptance_criteria: vec![],
                            task_type: TaskType::Chore,
                            tags: vec![],
                            required_tools: vec![],
                            priority: TaskPriority::Medium,
                            complexity: None,
                            crew: None,
                            status: TaskStatus::Backlog,
                        },
                        dedupe: DedupePolicy::SkipIfOpen,
                    })
                    .unwrap();
            }
            fixture.runtime.auto_task_toggle("disabled", false).unwrap();
            let baseline = Utc::now()
                .with_second(0)
                .unwrap()
                .with_nanosecond(0)
                .unwrap()
                - Duration::minutes(10);
            let cursor = orbit_core::application::auto_tasks::cursor_state_path(
                &fixture.runtime.paths().state_dir,
            );
            write_json(
                &cursor,
                json!({"definitions":{
                    "interval":{"baseline_at":baseline.to_rfc3339()},
                    "cron":{"baseline_at":baseline.to_rfc3339()},
                    "disabled":{"baseline_at":baseline.to_rfc3339()},
                }}),
            );
            fixture.job("schedule_fixture");
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            for (name, enabled) in [("armed", true), ("disabled", false), ("paused", true)] {
                fs::write(routines.join(format!("{name}.yaml")), format!(
                "schemaVersion: 1\nname: {name}\ndescription: HTTP projection\nenabled: {enabled}\ntrigger: {{cron: '* * * * *'}}\ntarget: job:schedule_fixture\n"
            )).unwrap();
            }
            orbit_core::application::routines::pause_routine(
                &fixture.global,
                "paused",
                "http-fixture",
            )
            .unwrap();
            let server = fixture.server(false);
            let before = Utc::now();
            let auto = json_ok(server.get("/api/auto-tasks?workspace=ws_http_fixture"));
            let after = Utc::now();
            let definitions = auto["definitions"].as_array().unwrap();
            assert_eq!(definitions.len(), 4, "{auto}");
            for (name, state, hypothetical) in [
                ("interval", "scheduled", false),
                ("cron", "scheduled", false),
                ("disabled", "disabled", true),
                ("unobserved", "never_observed", false),
            ] {
                let row = definitions.iter().find(|row| row["name"] == name).unwrap();
                assert_eq!(row["next_evaluation"]["state"], state, "{row}");
                assert_eq!(
                    row["next_evaluation"]["hypothetical"], hypothetical,
                    "{row}"
                );
                if name == "unobserved" {
                    assert!(row["next_evaluation"]["at"].is_null());
                } else {
                    let at = DateTime::parse_from_rfc3339(
                        row["next_evaluation"]["at"].as_str().unwrap(),
                    )
                    .unwrap()
                    .with_timezone(&Utc);
                    if name == "cron" {
                        assert!(
                            at > before && at <= after + Duration::minutes(1),
                            "next cron minute: {at}"
                        );
                        assert_eq!(at.second(), 0);
                    } else {
                        assert_eq!(
                            at,
                            baseline + Duration::minutes(60),
                            "interval retains first-observed anchor"
                        );
                    }
                }
            }
            let before = Utc::now();
            let report = json_ok(server.get("/api/routines?workspace=ws_http_fixture"));
            let after = Utc::now();
            let rows = report["routines"].as_array().unwrap();
            assert_eq!(rows.len(), 3, "{report}");
            for (name, state, hypothetical) in [
                ("armed", "scheduled", false),
                ("disabled", "disabled", true),
                ("paused", "paused", true),
            ] {
                let row = rows.iter().find(|row| row["name"] == name).unwrap();
                assert_eq!(row["next_evaluation"]["state"], state, "{row}");
                assert_eq!(
                    row["next_evaluation"]["hypothetical"], hypothetical,
                    "{row}"
                );
                let at =
                    DateTime::parse_from_rfc3339(row["next_evaluation"]["at"].as_str().unwrap())
                        .unwrap()
                        .with_timezone(&Utc);
                assert!(
                    at > before && at <= after + Duration::minutes(1),
                    "routine next cron minute: {at}"
                );
                assert_eq!(at.second(), 0);
            }
            // A corrupt host cursor is unavailable, not silently treated as never observed.
            fs::write(&cursor, "{broken").unwrap();
            let bad = json_ok(server.get("/api/auto-tasks?workspace=ws_http_fixture"));
            assert!(bad["cursor_state_error"].is_string());
            let interval = bad["definitions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["name"] == "interval")
                .unwrap();
            assert_eq!(interval["next_evaluation"]["state"], "unavailable");
            assert!(interval["next_evaluation"]["at"].is_null());
            assert_eq!(
                fs::read_to_string(cursor).unwrap(),
                "{broken",
                "GET must preserve corrupt evidence"
            );
        },
    );
}
