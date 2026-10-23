use std::fs;

use std::path::Path;

use chrono::{DateTime, Duration, Timelike, Utc};
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::AutoTaskAddParams;
use orbit_core::V2AuditEventInsertParams;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, TaskAddParams,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, blocked_workflow_failure_update};
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
fn friction_stderr_excerpts_bound_bytes_and_tolerate_missing_blobs() {
    isolated(
        "projections::friction_stderr_excerpts_bound_bytes_and_tolerate_missing_blobs",
        || {
            // The dashboard API's per-message cap for list projections.
            const CAP: usize = 2048;
            let fixture = Fixture::new();
            let blobs = BlobStore::new(fixture.runtime.data_root().join("state/audit/blobs"));
            let ts = "2026-04-10T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
            let cases = [
                ("oversized", "x".repeat(1024 * 1024), "x".repeat(CAP), true),
                ("exact-cap", "x".repeat(CAP), "x".repeat(CAP), false),
                ("over-cap", "x".repeat(CAP + 1), "x".repeat(CAP), true),
                (
                    "short",
                    "short stderr\n".into(),
                    "short stderr\n".into(),
                    false,
                ),
                (
                    "many-lines",
                    "line\n".repeat(500),
                    "line\n".repeat(500)[..CAP].to_string(),
                    true,
                ),
                (
                    "unicode",
                    format!("{}☃tail", "x".repeat(CAP - 1)),
                    "x".repeat(CAP - 1),
                    true,
                ),
                ("missing", String::new(), String::new(), false),
            ];
            for (id, content, _, _) in &cases {
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
            for (id, _, expected, truncated) in cases {
                let row = rows.iter().find(|row| row["command"] == id).unwrap();
                assert_eq!(row["stderr"], expected, "bounded stderr for {id}");
                assert_eq!(row["truncated"], truncated, "truncation flag for {id}");
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
            let path = "/api/diagnostics/friction?month=2026-04&limit=1&workspace=ws_http_fixture";
            // A neighboring dashboard has the same logical workspace ID but
            // an empty store. Its health response must not admit this child
            // before the child has bound and announced its own address.
            let empty_fixture = Fixture::new();
            let empty_server = empty_fixture.server(false);
            assert!(
                json_ok(empty_server.get(path))
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            let server = fixture.server(false);
            assert_ne!(server.origin, empty_server.origin);
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
fn cost_and_metrics_windows_scope_rows_and_timestamp_arithmetic() {
    isolated(
        "projections::cost_and_metrics_windows_scope_rows_and_timestamp_arithmetic",
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
                "ts":ts.to_rfc3339(),"job_run":"jrun-windows","step":"implement",
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
                    "/api/cost?window={window}&workspace=ws_http_fixture"
                )));
                let after = Utc::now();
                let metrics = json_ok(server.get(&format!(
                    "/api/diagnostics/metrics?since={window}&workspace=ws_http_fixture"
                )));
                assert_eq!(
                    metrics
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| row["retry_count"].as_u64().unwrap())
                        .sum::<u64>(),
                    expected,
                    "metrics must filter all month partitions before limiting: {window}"
                );
                assert_eq!(result["window"], window);
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
            let limited = json_ok(
                server.get("/api/diagnostics/metrics?since=all&limit=1&workspace=ws_http_fixture"),
            );
            assert_eq!(limited.as_array().unwrap().len(), 1);
            assert_eq!(
                limited[0]["retry_count"], 1,
                "newest eligible row wins across partitions"
            );
            assert!(
                json_ok(
                    server.get(
                        "/api/diagnostics/metrics?since=all&limit=0&workspace=ws_http_fixture"
                    )
                )
                .as_array()
                .unwrap()
                .is_empty()
            );
            assert_eq!(
                server
                    .get("/api/diagnostics/metrics?since=bogus&workspace=ws_http_fixture")
                    .status()
                    .as_u16(),
                400
            );
            assert_eq!(
                server
                    .get(
                        "/api/diagnostics/metrics?since=24h&month=2026-04&workspace=ws_http_fixture"
                    )
                    .status()
                    .as_u16(),
                400
            );
            let default = json_ok(server.get("/api/cost?workspace=ws_http_fixture"));
            assert_eq!(default["window"], "all");
            assert_eq!(
                server
                    .get("/api/cost?window=bogus&workspace=ws_http_fixture")
                    .status()
                    .as_u16(),
                400
            );
            // The retired scoreboard route answers with the cost body until
            // its sunset, and says so in the response headers.
            let alias = server.get("/api/scoreboard?window=24h&workspace=ws_http_fixture");
            assert_eq!(alias.headers()["deprecation"], "true");
            assert!(alias.headers().contains_key("sunset"));
            let alias = json_ok(alias);
            assert_eq!(alias["window"], "24h");
            assert!(alias["orchestration"].is_object());
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
                            context_files: Vec::new(),
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
            // Cron is evaluated in host-local time, so both payloads name that
            // zone for the dashboard to label triggers with.
            let host_offset = chrono::Local::now().offset().local_minus_utc();
            assert_eq!(auto["cron_zone"]["offset_seconds"], host_offset, "{auto}");
            assert!(
                auto["cron_zone"]["name"].is_string() || auto["cron_zone"]["name"].is_null(),
                "{auto}"
            );
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
            assert_eq!(report["cron_zone"], auto["cron_zone"], "{report}");
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
/// owner's routine is refused without a write, as is every other refused
/// selection, each under its own error code.
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
            assert_eq!(scheduled[0]["origin"], "workspace", "{report}");
            let owner_only = report["owner_only"].as_array().unwrap();
            assert_eq!(owner_only.len(), 1, "{report}");
            assert_eq!(owner_only[0]["name"], "replica-ship");
            assert_eq!(owner_only[0]["owner_machine"], "hm_fixture_remote");
            assert_eq!(
                fs::canonicalize(owner_only[0]["path"].as_str().unwrap()).unwrap(),
                fs::canonicalize(&ship_path).unwrap(),
                "{report}"
            );

            let toggle_from = |name: &str, source: &str, target: &str, expected: bool| {
                server.send(
                    "POST",
                    "/api/routines/toggle?workspace=ws_http_fixture",
                    json!({"name":name,"source":source,"target":target,
                        "machine_name":"http-fixture","expected_enabled":expected,"enabled":true}),
                )
            };
            let toggle = |name: &str, target: &str| toggle_from(name, "fixture", target, false);
            let gc_path = routines.join("replica-gc.yaml");
            let gc_before = fs::read(&gc_path).unwrap();
            for (response, status, code) in [
                (
                    toggle("missing", "job:worktree_gc_pipeline"),
                    404,
                    "routine_not_found",
                ),
                (
                    toggle_from("replica-gc", "elsewhere", "job:worktree_gc_pipeline", false),
                    409,
                    "workspace_mismatch",
                ),
                (
                    toggle("replica-gc", "job:workspace_ship_pipeline"),
                    409,
                    "target_mismatch",
                ),
                (
                    toggle_from("replica-gc", "fixture", "job:worktree_gc_pipeline", true),
                    409,
                    "stale_routine_state",
                ),
            ] {
                error_code(response, status, code);
                assert_eq!(fs::read(&gc_path).unwrap(), gc_before, "{code} wrote");
            }
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
                fs::read_to_string(&gc_path)
                    .unwrap()
                    .contains("enabled: true")
            );
        },
    );
}

/// [ORB-14173] Replica auto-task controls must match the scheduler's owner-only
/// boundary: the dashboard names the owner and neither toggle nor manual mint
/// mutates replica control-plane state.
#[test]
fn replica_auto_task_controls_explain_owner_authority_and_refuse_mutations() {
    isolated(
        "projections::replica_auto_task_controls_explain_owner_authority_and_refuse_mutations",
        || {
            let fixture = Fixture::new();
            fixture
                .runtime
                .auto_task_add(AutoTaskAddParams {
                    name: "qa-sweep".into(),
                    description: "Replica control boundary fixture".into(),
                    schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
                    template: AutoTaskTemplate {
                        title: "Should remain unminted".into(),
                        description: "An auto-task may not issue work from a replica.".into(),
                        acceptance_criteria: vec!["No task is minted.".into()],
                        task_type: TaskType::Chore,
                        tags: vec![],
                        required_tools: vec![],
                        context_files: Vec::new(),
                        priority: TaskPriority::Medium,
                        complexity: None,
                        crew: None,
                        status: TaskStatus::Backlog,
                    },
                    dedupe: DedupePolicy::SkipIfOpen,
                })
                .unwrap();
            let fixture = fixture.into_replica("hm_fixture_remote");
            let definition_path = fixture.work.join("auto_tasks/qa-sweep.yaml");
            let definition_before = fs::read(&definition_path).unwrap();
            let server = fixture.server(true);

            let listed = json_ok(server.get("/api/auto-tasks?workspace=ws_http_fixture"));
            assert_eq!(listed["controls_authorized"], false, "{listed}");
            for action in ["auto_task_toggle", "auto_task_mint"] {
                let capability = &listed["capabilities"][action];
                assert_eq!(capability["authorized"], false, "{action}: {listed}");
                assert!(
                    capability["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("hm_fixture_remote")),
                    "{action} refusal names the owner: {listed}"
                );
            }

            let refused_toggle = error_code(
                server.send(
                    "POST",
                    "/api/auto-tasks/toggle?workspace=ws_http_fixture",
                    json!({"name":"qa-sweep","expected_enabled":true,"enabled":false}),
                ),
                403,
                "capability_refused",
            );
            assert!(
                refused_toggle["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("hm_fixture_remote")),
                "{refused_toggle}"
            );
            let refused_mint = error_code(
                server.send(
                    "POST",
                    "/api/auto-tasks/mint?workspace=ws_http_fixture",
                    json!({"name":"qa-sweep","acknowledge_unconditional":true}),
                ),
                403,
                "capability_refused",
            );
            assert!(
                refused_mint["error"]
                    .as_str()
                    .is_some_and(|error| error.contains("hm_fixture_remote")),
                "{refused_mint}"
            );
            assert_eq!(fs::read(&definition_path).unwrap(), definition_before);
            assert!(
                fixture
                    .runtime
                    .list_tasks_by_tags(&["auto-task:qa-sweep".into()])
                    .unwrap()
                    .is_empty()
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
            assert_eq!(policy["policy_decisions"]["total"], summary["denials"]);
            assert_eq!(policy["policy_decisions"]["sql"], SQL_POLICY_COUNT);
            assert_eq!(policy["policy_decisions"]["v2"], V2_POLICY_COUNT);
            let tool_policy = json_ok(
                server
                    .get("/api/diagnostics/denials?kind=tool&since=24h&workspace=ws_http_fixture"),
            );
            let recent = tool_policy["recent_denials"].as_array().unwrap();
            assert_eq!(
                tool_policy["policy_decisions"], policy["policy_decisions"],
                "evidence filters must not change the top-bar population"
            );
            assert!(tool_policy["total"].as_i64().unwrap() < policy["total"].as_i64().unwrap());
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

#[test]
fn dashboard_memo_polls_report_payload_age() {
    isolated(
        "projections::dashboard_memo_polls_report_payload_age",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(false);
            for path in [
                "/api/audit/summary?since=24h&workspace=ws_http_fixture",
                "/api/cost?window=1h&workspace=ws_http_fixture",
                "/api/diagnostics/errors?since=24h&workspace=ws_http_fixture",
                "/api/diagnostics/friction?month=2026-04&workspace=ws_http_fixture",
            ] {
                let first = server.get(path);
                let first_age = first.headers()["age"].to_str().unwrap().to_owned();
                let first = json_ok(first);
                let repeat = server.get(path);
                let repeat_age = repeat.headers()["age"].to_str().unwrap().to_owned();
                assert!(
                    first_age.parse::<u64>().is_ok(),
                    "{path}: Age {first_age:?}"
                );
                assert!(
                    repeat_age.parse::<u64>().is_ok(),
                    "{path}: Age {repeat_age:?}"
                );
                assert_eq!(
                    json_ok(repeat),
                    first,
                    "{path}: a repeat poll inside the TTL is served from the memo"
                );
            }
        },
    );
}

/// A `github-pr` ref recorded with only its number links to the pull request
/// in the workspace's own GitHub repository, in the detail and on list rows.
/// A recorded page is kept; a ref whose id is not a PR number, or a workspace
/// whose origin is not on GitHub, stays unlinked.
#[test]
fn github_pr_refs_link_the_pull_request_from_the_origin_remote() {
    isolated(
        "projections::github_pr_refs_link_the_pull_request_from_the_origin_remote",
        || {
            let fixture = Fixture::new();
            let repo = fixture.path("repo");
            let git = |args: &[&str]| {
                let status = crate::git_test_support::command()
                    .args(args)
                    .current_dir(&repo)
                    .status()
                    .unwrap();
                assert!(status.success(), "git {args:?}");
            };
            git(&["init", "-q"]);
            git(&[
                "remote",
                "add",
                "origin",
                "git@github.com:constellation-works/orbit.git",
            ]);
            let server = fixture.server(true);
            let create = |refs: Value| {
                json_ok(server.send(
                    "POST",
                    "/api/tasks?workspace=ws_http_fixture",
                    json!({"title": "PR link fixture", "description": "Delivered work",
                    "acceptance_criteria": ["Link the PR"], "complexity": "low",
                    "external_refs": refs}),
                ))["id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            };
            let refs = |id: &str| {
                json_ok(server.get(&format!("/api/tasks/{id}?workspace=ws_http_fixture")))
                    ["external_refs"]
                    .clone()
            };
            let number_only = create(json!([
                {"system": "github-pr", "id": "4067"},
                {"system": "jira", "id": "ENG-1"},
            ]));
            let recorded = create(json!([
                {"system": "github-pr", "id": "12", "url": "https://github.example/o/r/pull/12"},
            ]));
            let not_a_number = create(json!([{"system": "github-pr", "id": "40/../67"}]));

            let linked = json!([
                {"system": "github-pr", "id": "4067",
                 "url": "https://github.com/constellation-works/orbit/pull/4067"},
                {"system": "jira", "id": "ENG-1"},
            ]);
            assert_eq!(refs(&number_only), linked);
            assert_eq!(
                refs(&recorded),
                json!([{"system": "github-pr", "id": "12",
                        "url": "https://github.example/o/r/pull/12"}])
            );
            assert_eq!(
                refs(&not_a_number),
                json!([{"system": "github-pr", "id": "40/../67"}])
            );
            let page = json_ok(server.get("/api/tasks?workspace=ws_http_fixture"));
            let row = page["items"]
                .as_array()
                .or_else(|| page.as_array())
                .unwrap()
                .iter()
                .find(|row| row["id"] == number_only.as_str())
                .cloned()
                .unwrap();
            assert_eq!(row["external_refs"], linked, "the list row links it too");

            git(&[
                "remote",
                "set-url",
                "origin",
                "https://gitlab.example/o/r.git",
            ]);
            assert_eq!(
                refs(&number_only),
                json!([{"system": "github-pr", "id": "4067"}, {"system": "jira", "id": "ENG-1"}]),
                "a remote off GitHub leaves the number unlinked"
            );
        },
    );
}

/// The dashboard serves the task JSON every read surface shares and adds only
/// its own extras: the detail is the tool host's `orbit.task.show` record plus
/// `status_transitions`, `job_run_navigable` and `readiness.preparing`, and a
/// list row carries the same base keys as `orbit.task.list` minus the prose
/// bodies. A crew this host no longer configures reads as `crew_unresolved` on
/// the detail, as it does on the CLI and tool host, instead of being swapped
/// for the default crew.
#[test]
fn task_detail_and_list_rows_serve_the_shared_task_projection() {
    isolated(
        "projections::task_detail_and_list_rows_serve_the_shared_task_projection",
        || {
            let fixture = Fixture::new();
            let server = fixture.server(true);
            let create = |server: &super::support::Server, body: Value| {
                json_ok(server.send("POST", "/api/tasks?workspace=ws_http_fixture", body))["id"]
                    .as_str()
                    .unwrap()
                    .to_string()
            };
            let id = create(
                &server,
                json!({"title": "Projection parity", "description": "Shared JSON",
                "acceptance_criteria": ["Match"], "complexity": "low"}),
            );
            let source = fixture.runtime.paths().repo_root.join("parity.txt");
            fs::write(&source, "parity artifact\n").unwrap();
            fixture
                .runtime
                .run_tool(
                    "orbit.task.artifact.put",
                    json!({"id": id, "source_path": source, "path": "reports/parity.txt",
                    "model": "codex"}),
                )
                .unwrap();

            let mut detail =
                json_ok(server.get(&format!("/api/tasks/{id}?workspace=ws_http_fixture")));
            let extras = detail.as_object_mut().unwrap();
            for key in ["status_transitions", "job_run_navigable"] {
                assert!(extras.remove(key).is_some(), "dashboard extra `{key}`");
            }
            assert!(
                extras["readiness"]
                    .as_object_mut()
                    .unwrap()
                    .remove("preparing")
                    .is_some()
            );
            let shown = fixture
                .runtime
                .run_tool("orbit.task.show", json!({"id": id}))
                .unwrap();
            assert_eq!(detail, shown, "the detail is the shared task record");
            assert!(detail.as_object().unwrap().contains_key("job_run_machine"));
            assert_eq!(detail["artifacts"][0]["size_bytes"], 16);

            let page = json_ok(server.get("/api/tasks?workspace=ws_http_fixture"));
            let row = page["items"]
                .as_array()
                .or_else(|| page.as_array())
                .unwrap()
                .iter()
                .find(|row| row["id"] == id.as_str())
                .cloned()
                .unwrap();
            let listed = fixture
                .runtime
                .run_tool("orbit.task.list", json!({}))
                .unwrap();
            let tool_row = listed["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == id.as_str())
                .unwrap();
            for (key, value) in tool_row.as_object().unwrap() {
                match key.as_str() {
                    "description" | "plan" | "execution_summary" | "acceptance_criteria" => {
                        assert!(row.get(key).is_none(), "a summary row omits `{key}`");
                    }
                    "readiness" => assert_eq!(row[key]["gaps"], value["gaps"]),
                    _ => assert_eq!(&row[key], value, "list row `{key}`"),
                }
            }
            assert!(row.as_object().unwrap().contains_key("job_run_machine"));
            drop(server);

            // Author a task while its crew exists, then serve it from a host
            // configuration that no longer defines that crew.
            let global_path = fixture.global.join("config.toml");
            let baseline = fs::read_to_string(&global_path).unwrap();
            let configured = concat!(
                "\n[workflow]\ndefault_crew = \"keep\"\n",
                "\n[crews.keep]\nprovider = \"claude\"\nmodel = \"keep-model\"\n",
            );
            fs::write(
                &global_path,
                format!(
                    "{baseline}{configured}\n[crews.gone]\nprovider = \"claude\"\nmodel = \"gone-model\"\n"
                ),
            )
            .unwrap();
            let server = fixture.server(true);
            let stale = create(
                &server,
                json!({"title": "Stale crew", "description": "Crew removed later",
                "acceptance_criteria": ["Stay readable"], "complexity": "low", "crew": "gone"}),
            );
            drop(server);
            fs::write(&global_path, format!("{baseline}{configured}")).unwrap();
            let server = fixture.server(true);
            let detail =
                json_ok(server.get(&format!("/api/tasks/{stale}?workspace=ws_http_fixture")));
            assert_eq!(detail["crew"], "gone");
            assert!(
                detail.get("resolved_crew").is_none() && detail.get("crew_model").is_none(),
                "an unresolvable crew is not swapped for the default: {detail}"
            );
            assert!(
                detail["crew_unresolved"]
                    .as_str()
                    .is_some_and(|reason| !reason.is_empty()),
                "the detail names why the crew is unresolved: {detail}"
            );
        },
    );
}

/// A run-blocked task with an escalation recorded without another status change.
fn block_with_final_recovery(
    runtime: &orbit_core::OrbitRuntime,
    repo: &Path,
    id: &str,
    run_id: &str,
) {
    runtime
        .apply_task_automation_update(
            id,
            TaskAutomationUpdate {
                job_run_id: Some(run_id.into()),
                ..blocked_workflow_failure_update(
                    "task_pr_pipeline",
                    run_id,
                    Some("STEP_FAILED"),
                    Some("step `implement` completed with success=false"),
                )
            },
        )
        .unwrap();
    let task = runtime.get_task(id).unwrap();
    let observed = runtime.final_recovery_revision(&task).unwrap();
    let outcome = runtime
        .apply_final_recovery(
            &FinalRecoveryRequest {
                task_id: id.to_string(),
                run_id: run_id.to_string(),
                observed,
                repo_root: repo.to_path_buf(),
                base_ref: "main".into(),
                completion: FinalRecoveryCompletion::Review,
                requeue_bound: Default::default(),
            },
            Some(&json!({
                "decision": "escalate",
                "diagnosis": "the failure needs an operator",
                "human_action": "move the task back to backlog after fixing the cause",
            })),
        )
        .unwrap();
    assert_eq!(outcome, FinalRecoveryOutcome::Escalated { reason: None });
}

/// `GET /api/tasks/:id` serves the shared show record plus the dashboard extras.
/// A blocked task's explanation and guidance match `orbit.task.show`.
/// List rows and mutation responses omit show-only guidance.
#[test]
fn task_detail_includes_blocked_guidance_and_list_rows_omit_it() {
    isolated(
        "projections::task_detail_includes_blocked_guidance_and_list_rows_omit_it",
        || {
            let fixture = Fixture::new();
            let blocked = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Blocked guidance".into(),
                    acceptance_criteria: vec!["Show the next step".into()],
                    ..Default::default()
                })
                .unwrap()
                .id;
            let plain = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Plain detail".into(),
                    acceptance_criteria: vec!["Stay quiet".into()],
                    ..Default::default()
                })
                .unwrap()
                .id;
            let run_id = "jrun-http-guidance";
            block_with_final_recovery(&fixture.runtime, &fixture.path("repo"), &blocked, run_id);

            let server = fixture.server(true);
            let raw =
                json_ok(server.get(&format!("/api/tasks/{blocked}?workspace=ws_http_fixture")));
            assert!(raw.get("status_transitions").is_some());
            assert!(raw.get("job_run_navigable").is_some());
            assert_eq!(
                raw["next_step"],
                json!({
                    "run_id": run_id,
                    "resume_command": null,
                    "requeue_command": format!("orbit task update {blocked} --status backlog"),
                })
            );
            assert_eq!(raw["final_recovery"]["run_id"], run_id);
            assert_eq!(raw["final_recovery"]["decision"], "escalate");
            assert_eq!(raw["final_recovery"]["outcome"], "escalated");
            assert_eq!(raw["block"]["reason"], "the failure needs an operator");
            assert_eq!(raw["block"]["run_id"], run_id);
            assert_eq!(raw["block"]["at"], raw["final_recovery"]["at"]);
            assert_eq!(
                raw["block"]["next_action"],
                "move the task back to backlog after fixing the cause"
            );
            assert!(raw["final_recovery"]["at"].as_str().unwrap().contains('T'));

            let mut detail = raw.clone();
            let stripped = detail.as_object_mut().unwrap();
            for key in ["status_transitions", "job_run_navigable"] {
                assert!(stripped.remove(key).is_some(), "dashboard extra `{key}`");
            }
            if let Some(readiness) = stripped.get_mut("readiness").and_then(Value::as_object_mut) {
                readiness.remove("preparing");
            }
            let shown = fixture
                .runtime
                .run_tool("orbit.task.show", json!({"id": blocked}))
                .unwrap();
            assert_eq!(detail, shown, "the detail is the shared show record");
            for extra in ["status_transitions", "job_run_navigable"] {
                assert!(
                    shown.get(extra).is_none(),
                    "the tool show does not add dashboard extra `{extra}`"
                );
            }

            let page = json_ok(server.get("/api/tasks?workspace=ws_http_fixture"));
            let rows = page["items"]
                .as_array()
                .or_else(|| page.as_array())
                .unwrap();
            for id in [&blocked, &plain] {
                let row = rows.iter().find(|row| row["id"] == id.as_str()).unwrap();
                assert!(
                    row.get("next_step").is_none() && row.get("final_recovery").is_none(),
                    "a list row omits show guidance: {row}"
                );
            }
            let listed = fixture
                .runtime
                .run_tool("orbit.task.list", json!({}))
                .unwrap();
            let tool_row = listed["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["id"] == blocked.as_str())
                .unwrap();
            assert!(
                tool_row.get("next_step").is_none() && tool_row.get("final_recovery").is_none(),
                "orbit.task.list omits show guidance: {tool_row}"
            );

            let updated = json_ok(server.send(
                "PATCH",
                &format!("/api/tasks/{blocked}?workspace=ws_http_fixture"),
                json!({"comment": "operator looked at the block"}),
            ));
            assert!(updated.get("status_transitions").is_some());
            assert!(
                updated.get("next_step").is_none() && updated.get("final_recovery").is_none(),
                "a mutation response omits show guidance: {updated}"
            );
            let again =
                json_ok(server.get(&format!("/api/tasks/{blocked}?workspace=ws_http_fixture")));
            assert!(again.get("next_step").is_some() && again.get("final_recovery").is_some());

            let quiet =
                json_ok(server.get(&format!("/api/tasks/{plain}?workspace=ws_http_fixture")));
            assert!(quiet.get("next_step").is_none() && quiet.get("final_recovery").is_none());
            assert!(quiet.get("block").is_none());
        },
    );
}

/// Pilot warnings belong to the durable task payload even without a drain.
/// Advisory findings are excluded; a clean pilot or leaving proposed clears them.
#[test]
fn task_payloads_expose_durable_holding_pilot_warnings() {
    isolated(
        "projections::task_payloads_expose_durable_holding_pilot_warnings",
        || {
            use orbit_core::application::task::TaskUpdateParams;
            use orbit_types::task::{
                TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME, TaskComplexity, TaskDecision,
                TaskEventRowV2,
            };
            use orbit_types::workflow::{JobRunState, PipelineState};
            use std::io::Write;
            let fixture = Fixture::new();
            let id = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Pilot warning fixture".into(),
                    complexity: TaskComplexity::Low,
                    tags: vec!["no-diff-expected".into()],
                    description: "Durable findings".into(),
                    acceptance_criteria: vec!["Expose full findings".into()],
                    ..Default::default()
                })
                .unwrap()
                .id;
            let record = |operation: char, assessment: Value| {
                fixture
                    .runtime
                    .update_task_as_human(
                        &id,
                        TaskUpdateParams {
                            comment: Some(format!(
                                "operation_id={}\n{}",
                                operation.to_string().repeat(64),
                                json!({"assessment": assessment})
                            )),
                            ..Default::default()
                        },
                        "task-pilot".into(),
                    )
                    .unwrap();
                // Seed the typed automation event paired with this pilot receipt.
                let dir = super::codebase::bundle_dir(&fixture.path(""), &id).unwrap();
                let path = dir.join(TASK_EVENTS_FILE_NAME);
                let count = fs::read_to_string(&path).unwrap().lines().count();
                let event = TaskEventRowV2 {
                    schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                    event_id: format!("EV-{:04}", count + 1),
                    at: Utc::now(),
                    by: "task-pilot".into(),
                    event_type: "task_pilot_applied".into(),
                    note: None,
                    from_status: None,
                    to_status: None,
                    operation_id: Some(operation.to_string().repeat(64)),
                    decision: (operation != 'b').then(|| TaskDecision::DrainApprovalHeld {
                        classification: "warnings".into(),
                    }),
                };
                let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
                writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
            };
            let assessment = json!({
                "utility_warnings": [{"text": "Confirm the operator workflow.\nKeep this second line.", "holds_promotion": true}],
                "surface_warnings": [{"text": "Optional polish", "holds_promotion": false}, "Update the task surface."],
                "validation_tool_warnings": ["Browser evidence required."],
            });
            let expected = json!([
                {"kind": "utility_warnings", "text": "Confirm the operator workflow.\nKeep this second line."},
                {"kind": "surface_warnings", "text": "Update the task surface."},
                {"kind": "validation_tool_warnings", "text": "Browser evidence required."},
            ]);
            record('a', assessment.clone());
            let server = fixture.server(true);
            let assert_payloads = |expected: &Value| {
                let detail =
                    json_ok(server.get(&format!("/api/tasks/{id}?workspace=ws_http_fixture")));
                assert_eq!(&detail["pilot_warnings"], expected);
                let page = json_ok(server.get("/api/tasks?workspace=ws_http_fixture"));
                let rows = page["items"]
                    .as_array()
                    .or_else(|| page.as_array())
                    .unwrap();
                let row = rows.iter().find(|row| row["id"] == id).unwrap();
                assert_eq!(&row["pilot_warnings"], expected);
                let shown = fixture
                    .runtime
                    .run_tool(
                        "orbit.task.show",
                        json!({"id": id, "fields": ["pilot_warnings"]}),
                    )
                    .unwrap();
                assert_eq!(&shown, expected);
                let listed = fixture
                    .runtime
                    .run_tool("orbit.task.list", json!({}))
                    .unwrap();
                let row = listed["tasks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["id"] == id)
                    .unwrap();
                assert_eq!(&row["pilot_warnings"], expected);
            };
            assert_payloads(&expected);
            fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Newer clean task".into(),
                    ..Default::default()
                })
                .unwrap();
            // Warning filtering happens before pagination and keeps accurate totals.
            for endpoint in ["/api/tasks", "/api/tasks/all"] {
                let filtered = json_ok(server.get(&format!(
                    "{endpoint}?workspace=ws_http_fixture&pilot=warnings&limit=1"
                )));
                assert_eq!(filtered["total"], 1, "{filtered}");
                assert_eq!(filtered["items"][0]["id"], id);
                assert!(filtered["next_cursor"].is_null(), "{filtered}");
            }
            let mut drain = fixture.seed_run(
                "jrun-pilot-warning-view",
                "workspace_auto_pipeline",
                JobRunState::Running,
            );
            drain.input = Some(json!({"approve_proposed": true, "max_active_leaf_runs": 4}));
            fixture.save_run(&drain);
            fixture
                .runtime
                .write_run_state(
                    &drain.run_id,
                    &PipelineState::new(
                        drain.run_id.clone(),
                        drain.job_id.clone(),
                        drain.input.clone().unwrap(),
                    ),
                )
                .unwrap();
            let readiness = fixture
                .runtime
                .workspace_auto_readiness(&[], None, 500, &[])
                .unwrap();
            let held = readiness["approvals"]["held"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["task_id"] == id)
                .unwrap();
            assert_eq!(
                held["findings"], expected,
                "CLI readiness reads the task's latest pilot source"
            );
            let card =
                json_ok(server.get("/api/workflows/auto/readiness?workspace=ws_http_fixture"));
            assert_eq!(card["approvals"]["held_by_reason"]["warnings"], 1);
            let held = card["approvals"]["held"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["task_id"] == id)
                .unwrap();
            assert!(
                held.get("findings").is_none(),
                "the card's HTTP payload needs no finding prose"
            );
            // Ordinary operator comments do not erase the latest verification.
            json_ok(server.send(
                "PATCH",
                &format!("/api/tasks/{id}?workspace=ws_http_fixture"),
                json!({"comment": "Investigating"}),
            ));
            assert_payloads(&expected);
            record(
                'b',
                json!({"utility_warnings": [], "surface_warnings": [], "validation_tool_warnings": []}),
            );
            assert_payloads(&json!([]));
            record('c', assessment);
            assert_payloads(&expected);
            json_ok(server.send(
                "POST",
                &format!("/api/tasks/{id}/approve?workspace=ws_http_fixture"),
                json!({}),
            ));
            assert_payloads(&json!([]));
        },
    );
}
