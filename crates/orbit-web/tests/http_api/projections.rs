use std::fs;

use chrono::{DateTime, Duration, Timelike, Utc};
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::AutoTaskAddParams;
use orbit_core::V2AuditEventInsertParams;
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use serde_json::{Value, json};

use super::support::{Fixture, error_code, isolated, json_ok, write_json};

fn seed_cli_failure(fixture: &Fixture, id: &str, ts: DateTime<Utc>, blob_ref: &str) {
    fixture
        .runtime
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: fixture.runtime.workspace_id().unwrap(),
            event_id: id.into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: "cli_invocation_finished".into(),
            ts,
            run_id: "jrun-friction-fixture".into(),
            agent_identity: "http-fixture".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: json!({
                "event_id": id,
                "ts": ts.to_rfc3339(),
                "body_kind": "cli_invocation_finished",
                "run_id": "jrun-friction-fixture",
                "step_id": "implement",
                "provider": id,
                "exit_code": 1,
                "stderr_blob_ref": blob_ref,
            })
            .to_string(),
        })
        .unwrap();
}

#[test]
fn friction_stderr_previews_bound_bytes_and_lines_and_tolerate_missing_blobs() {
    isolated(
        "projections::friction_stderr_previews_bound_bytes_and_lines_and_tolerate_missing_blobs",
        || {
            let fixture = Fixture::new();
            let blobs = BlobStore::new(fixture.runtime.data_root().join("state/audit/blobs"));
            let ts = "2026-04-10T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
            let cases = [
                (
                    "oversized",
                    "x".repeat(1024 * 1024),
                    format!("{}\n[truncated]", "x".repeat(8192)),
                ),
                ("exact-cap", "x".repeat(8192), "x".repeat(8192)),
                ("short", "short stderr\n".into(), "short stderr\n".into()),
                (
                    "many-lines",
                    "line\n".repeat(121),
                    format!("{}\n[truncated]", "line\n".repeat(120)),
                ),
                (
                    "unicode",
                    format!("{}☃tail", "x".repeat(8191)),
                    format!("{}\n[truncated]", "x".repeat(8191)),
                ),
                ("missing", String::new(), String::new()),
            ];
            for (id, content, _) in &cases {
                let blob_ref = if *id == "missing" {
                    "0".repeat(64)
                } else {
                    blobs.write(content.as_bytes()).unwrap()
                };
                seed_cli_failure(&fixture, id, ts, &blob_ref);
            }
            let server = fixture.server(false);
            let result =
                json_ok(server.get(
                    "/api/diagnostics/friction?month=2026-04&limit=20&workspace=ws_http_fixture",
                ));
            let rows = result.as_array().unwrap();
            assert_eq!(rows.len(), cases.len());
            for (id, _, expected) in cases {
                let row = rows.iter().find(|row| row["command"] == id).unwrap();
                assert_eq!(row["stderr"], expected, "bounded stderr for {id}");
                assert_eq!(row["step"], "implement");
                assert_eq!(row["exit_code"], 1);
            }
        },
    );
}

#[test]
fn friction_polls_reuse_projection_without_mixing_months_or_limits() {
    isolated(
        "projections::friction_polls_reuse_projection_without_mixing_months_or_limits",
        || {
            let fixture = Fixture::new();
            let blobs = BlobStore::new(fixture.runtime.data_root().join("state/audit/blobs"));
            let blob_ref = blobs.write(b"cached stderr").unwrap();
            let ts = "2026-04-10T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
            seed_cli_failure(&fixture, "first", ts, &blob_ref);
            let server = fixture.server(false);
            let path = "/api/diagnostics/friction?month=2026-04&limit=1&workspace=ws_http_fixture";
            let first = json_ok(server.get(path));
            assert_eq!(first.as_array().unwrap().len(), 1);
            assert_eq!(first[0]["command"], "first");

            // If a second poll rescans the audit store it will return the new
            // event, rather than the previously computed projection.
            seed_cli_failure(&fixture, "second", ts + Duration::seconds(1), &blob_ref);
            assert_eq!(json_ok(server.get(path)), first, "poll must reuse its memo");
            let two =
                json_ok(server.get(
                    "/api/diagnostics/friction?month=2026-04&limit=2&workspace=ws_http_fixture",
                ));
            assert_eq!(two.as_array().unwrap().len(), 2);
            assert_eq!(two[0]["command"], "second");
            assert_eq!(two[1]["command"], "first");
            let previous =
                json_ok(server.get(
                    "/api/diagnostics/friction?month=2026-03&limit=1&workspace=ws_http_fixture",
                ));
            assert!(
                previous.as_array().unwrap().is_empty(),
                "month has its own memo"
            );
            assert_eq!(
                server
                    .get("/api/diagnostics/friction?month=invalid&workspace=ws_http_fixture")
                    .status()
                    .as_u16(),
                400,
            );
        },
    );
}

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

/// [ORB-14173] On a replica the dashboard lists the worktree GC routine as
/// toggleable and the owner's work apart with the owner named; toggling the
/// owner's routine is refused without a write.
#[test]
fn replica_routines_project_only_worktree_gc_as_toggleable() {
    isolated(
        "projections::replica_routines_project_only_worktree_gc_as_toggleable",
        || {
            let fixture = Fixture::replica_of("hm_fixture_remote");
            fixture.job("worktree_gc_pipeline");
            fixture.job("workspace_ship_pipeline");
            let routines = fixture.work.join("routines");
            fs::create_dir_all(&routines).unwrap();
            for (name, job) in [
                ("replica-gc", "worktree_gc_pipeline"),
                ("replica-ship", "workspace_ship_pipeline"),
            ] {
                fs::write(
                    routines.join(format!("{name}.yaml")),
                    format!(
                        "schemaVersion: 1\nname: {name}\nenabled: false\ntrigger: {{cron: '* * * * *'}}\ntarget: job:{job}\n"
                    ),
                )
                .unwrap();
            }
            let ship_path = routines.join("replica-ship.yaml");
            let ship_before = fs::read(&ship_path).unwrap();
            let server = fixture.server(true);
            let report = json_ok(server.get("/api/routines?workspace=ws_http_fixture"));
            let scheduled = report["routines"].as_array().unwrap();
            assert_eq!(scheduled.len(), 1, "{report}");
            assert_eq!(scheduled[0]["name"], "replica-gc", "{report}");
            let owner_only = report["owner_only"].as_array().unwrap();
            assert_eq!(owner_only.len(), 1, "{report}");
            assert_eq!(owner_only[0]["name"], "replica-ship");
            assert_eq!(owner_only[0]["owner_machine"], "hm_fixture_remote");

            let toggle = |name: &str, target: &str| {
                server.send(
                    "POST",
                    "/api/routines/toggle?workspace=ws_http_fixture",
                    json!({"name":name,"source":"fixture","target":target,
                        "machine_name":"http-fixture","expected_enabled":false,"enabled":true}),
                )
            };
            let refused = error_code(
                toggle("replica-ship", "job:workspace_ship_pipeline"),
                409,
                "owner_authority",
            );
            assert!(
                refused["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("hm_fixture_remote")),
                "{refused}"
            );
            assert_eq!(fs::read(&ship_path).unwrap(), ship_before);
            let enabled = json_ok(toggle("replica-gc", "job:worktree_gc_pipeline"));
            assert_eq!(enabled["changed"], true, "{enabled}");
            assert!(
                fs::read_to_string(routines.join("replica-gc.yaml"))
                    .unwrap()
                    .contains("enabled: true")
            );
        },
    );
}

#[path = "../../../orbit-store/tests/fixtures/policy_denials.rs"]
mod policy_denials_fixture;

#[test]
fn policy_kpi_counts_decisions_and_preserves_refusal_evidence() {
    isolated(
        "projections::policy_kpi_counts_decisions_and_preserves_refusal_evidence",
        || {
            use policy_denials_fixture::{RAW_DENIED_COUNT, SQL_POLICY_COUNT, V2_POLICY_COUNT};
            let fixture = Fixture::new();
            policy_denials_fixture::seed(&fixture.runtime);
            let stats = fixture.runtime.audit_policy_denial_stats(None).unwrap();
            assert_eq!(stats.sql_denied, SQL_POLICY_COUNT);
            assert_eq!(stats.v2_denied, V2_POLICY_COUNT);
            let counts: std::collections::BTreeMap<_, _> = stats.by_operation.into_iter().collect();
            assert_eq!(
                counts["orbit.workflow.run.show"], 1,
                "legacy authorization/tool pair is one attempt"
            );
            assert_eq!(
                counts["orbit.command.exec"], 2,
                "MCP request IDs are scoped to sessions"
            );
            assert_eq!(
                counts["orbit.task.locks.reserve"], 1,
                "only the capability refusal counts, not contention"
            );
            assert_eq!(
                counts["orbit.drain.claim.settle"], 1,
                "only the capability refusal counts, not owner configuration"
            );
            assert_eq!(counts["fs.read"], 1);
            assert_eq!(counts["proc.spawn"], 1);
            let server = fixture.server(false);
            let summary =
                json_ok(server.get("/api/audit/summary?since=24h&workspace=ws_http_fixture"));
            assert_eq!(summary["denials_sql"], SQL_POLICY_COUNT);
            assert_eq!(summary["denials_v2"], V2_POLICY_COUNT);
            assert_eq!(summary["denials"], SQL_POLICY_COUNT + V2_POLICY_COUNT);
            let policy =
                json_ok(server.get("/api/diagnostics/denials?since=24h&workspace=ws_http_fixture"));
            assert_eq!(policy["total"], RAW_DENIED_COUNT + V2_POLICY_COUNT);
            let tool_policy = json_ok(
                server
                    .get("/api/diagnostics/denials?kind=tool&since=24h&workspace=ws_http_fixture"),
            );
            let recent = tool_policy["recent_denials"].as_array().unwrap();
            assert!(
                recent
                    .iter()
                    .any(|row| row["denial_kind"] == "claim_settlement_refusal")
            );
            assert!(
                recent
                    .iter()
                    .any(|row| row["denial_kind"] == "task_lock_reserve")
            );
            let raw = json_ok(server.get("/api/audit?status=denied&workspace=ws_http_fixture"));
            assert_eq!(raw.as_array().unwrap().len(), RAW_DENIED_COUNT as usize);
        },
    );
}
