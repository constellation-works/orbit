use std::fs;

use std::path::Path;

use chrono::{DateTime, Duration, Timelike, Utc};
use orbit_cmd::registry_runtime::RegisteredRuntimeFactory;
use orbit_common::storage::blob_store::BlobStore;
use orbit_core::AutoTaskAddParams;
use orbit_core::InvocationInsertParams;
use orbit_core::V2AuditEventInsertParams;
use orbit_core::application::auto_tasks::cursor_state_path;
use orbit_core::application::routines::pause_routine;
use orbit_core::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, TaskAddParams,
    TaskUpdateParams,
};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, blocked_workflow_failure_update};
use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME, TaskComplexity, TaskDecision,
    TaskEventRowV2,
};
use orbit_types::task::{TaskPriority, TaskStatus, TaskType};
use orbit_types::telemetry::{InvocationTrace, TokenUsage, ToolCallTrace};
use orbit_types::workflow::{AutoTaskSchedule, AutoTaskTemplate, DedupePolicy};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};
use std::io::Write;

use super::support::{Fixture, Server, error_code, isolated, json_ok, write_json};

macro_rules! assert_fields {
    ($row:expr, $($key:literal: $val:expr),+ $(,)?) => {{
        let row = &$row;
        $(assert_eq!(row[$key], $val, "field `{}` in {row}", $key);)+
    }};
}
/// Every projections scenario gets its own authority-scrubbed child; derive
/// the exact libtest name here so selectors cannot drift from isolation.
macro_rules! projections_test {
    ($name:ident, $fixture:ident, $init:expr, $body:block) => {
        #[test]
        fn $name() {
            isolated(concat!("projections::", stringify!($name)), || {
                let $fixture = $init;
                $body
            });
        }
    };
}

/// Equality/condition checks that keep the case name in the failure message.
macro_rules! check_eq {
    ($actual:expr => $expected:expr; $($msg:tt)*) => {
        assert_eq!($actual, $expected, $($msg)*)
    };
}
macro_rules! check {
    ($cond:expr; $($msg:tt)*) => {
        assert!($cond, $($msg)*)
    };
}

fn get_ok(server: &Server, path: &str) -> Value {
    json_ok(server.get(path))
}
fn task_detail(server: &Server, id: &str) -> Value {
    get_ok(
        server,
        &format!("/api/tasks/{id}?workspace=ws_http_fixture"),
    )
}
fn task_page(server: &Server) -> Value {
    get_ok(server, "/api/tasks?workspace=ws_http_fixture")
}
fn rows_of(page: &Value) -> &Vec<Value> {
    page["items"]
        .as_array()
        .or_else(|| page.as_array())
        .unwrap()
}
fn row_of<'a>(page: &'a Value, id: &str) -> &'a Value {
    rows_of(page).iter().find(|row| row["id"] == id).unwrap()
}
fn show_task(fixture: &Fixture, id: &str) -> Value {
    let rt = &fixture.runtime;
    rt.run_tool("orbit.task.show", json!({"id": id})).unwrap()
}
fn listed_tasks(fixture: &Fixture) -> Value {
    let rt = &fixture.runtime;
    rt.run_tool("orbit.task.list", json!({})).unwrap()
}
fn tool_row<'a>(listed: &'a Value, id: &str) -> &'a Value {
    find_row(listed["tasks"].as_array().unwrap(), "id", id)
}
fn status_of(server: &Server, path: &str) -> u16 {
    server.get(path).status().as_u16()
}
fn parse_ts(value: &Value) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value.as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc)
}
fn assert_next(row: &Value, state: &str, hypothetical: bool) {
    assert_fields!(row["next_evaluation"], "state": json!(state), "hypothetical": json!(hypothetical));
}
/// Template text: (title, add description, template description, criteria).
fn seed_auto_task(
    fixture: &Fixture,
    name: &str,
    schedule: AutoTaskSchedule,
    text: (String, &str, &str, Vec<String>),
) {
    let (title, description, template_description, criteria) = text;
    let template = AutoTaskTemplate {
        title,
        description: template_description.into(),
        acceptance_criteria: criteria,
        task_type: TaskType::Chore,
        tags: vec![],
        required_tools: vec![],
        context_files: Vec::new(),
        priority: TaskPriority::Medium,
        complexity: None,
        crew: None,
        status: TaskStatus::Backlog,
    };
    fixture
        .runtime
        .auto_task_add(AutoTaskAddParams {
            name: name.into(),
            description: description.into(),
            schedule,
            template,
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap();
}
fn seed_schedule_task(fixture: &Fixture, name: &str, schedule: AutoTaskSchedule) {
    let text = (
        format!("Projection {name}"),
        "projection fixture",
        "HTTP schedule fixture",
        vec![],
    );
    seed_auto_task(fixture, name, schedule, text);
}
fn record_pilot_receipt(
    fixture: &Fixture,
    id: &str,
    op: char,
    assessment: Value,
    decision: Option<TaskDecision>,
) {
    let op_id = op.to_string().repeat(64);
    let comment = Some(format!(
        "operation_id={op_id}\n{}",
        json!({"assessment": assessment})
    ));
    fixture
        .runtime
        .update_task_as_human(
            id,
            TaskUpdateParams {
                comment,
                ..Default::default()
            },
            "task-pilot".into(),
        )
        .unwrap();
    let path = super::codebase::bundle_dir(&fixture.path(""), id)
        .unwrap()
        .join(TASK_EVENTS_FILE_NAME);
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
        operation_id: Some(op_id),
        decision,
    };
    let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{}", serde_json::to_string(&event).unwrap()).unwrap();
}
fn assert_pilot_warnings(fixture: &Fixture, server: &Server, id: &str, expected: &Value) {
    check_eq!(&task_detail(server, id)["pilot_warnings"] => expected; "detail pilot_warnings for {id}");
    check_eq!(&row_of(&task_page(server), id)["pilot_warnings"] => expected; "list pilot_warnings for {id}");
    let rt = &fixture.runtime;
    let shown = rt
        .run_tool(
            "orbit.task.show",
            json!({"id": id, "fields": ["pilot_warnings"]}),
        )
        .unwrap();
    check_eq!(&shown => expected; "field-scoped show pilot_warnings for {id}");
    check_eq!(&show_task(fixture, id)["pilot_warnings"] => expected; "show pilot_warnings for {id}");
    check_eq!(&tool_row(&listed_tasks(fixture), id)["pilot_warnings"] => expected; "tool list pilot_warnings for {id}");
}

fn find_row<'a>(rows: &'a [Value], key: &str, name: &str) -> &'a Value {
    rows.iter().find(|row| row[key] == name).unwrap()
}
fn no_keys(value: &Value, keys: &[&str], context: &str) {
    for key in keys {
        assert!(value.get(key).is_none(), "{context} omits `{key}`: {value}");
    }
}
fn strip(value: &mut Value, keys: &[&str]) {
    let object = value.as_object_mut().unwrap();
    for key in keys {
        assert!(object.remove(*key).is_some(), "dashboard extra `{key}`");
    }
}
fn len_eq(rows: &[Value], expected: usize, context: &str) {
    assert_eq!(rows.len(), expected, "{context}: {rows:?}");
}
fn assert_no_activity(rows: &[Value], activity: &str, context: &str) {
    check!(rows.iter().all(|row| row["activity_id"] != activity); "{context} leaked into rows: {rows:?}");
}
fn assert_avg(row: &Value, key: &str, expected: f64) {
    check!((row[key].as_f64().unwrap() - expected).abs() < 1e-9; "avg {key} preserves selected population: {row}");
}
fn count_eq(c: &std::collections::BTreeMap<String, i64>, k: &str, n: i64, why: &str) {
    assert_eq!(c[k], n, "{why}");
}
fn assert_contains(value: &Value, key: &str, needle: &str) {
    check!(value[key].as_str().is_some_and(|text| text.contains(needle)); "{key} must name {needle}: {value}");
}
fn seed_null_scope(
    rt: &orbit_core::OrbitRuntime,
    fixture: &Fixture,
    run: &str,
    activity: &str,
    agent: &str,
    model: &str,
    tool: &str,
) {
    seed_scope(rt, run, activity, agent, model, tool, 1_000, 1_000, 999);
    null_scope_workspace(fixture, run);
}
fn add_titled_task(fixture: &Fixture, title: &str, criteria: &str) -> String {
    add_task(
        fixture,
        TaskAddParams {
            title: title.into(),
            acceptance_criteria: vec![criteria.into()],
            ..Default::default()
        },
    )
    .id
}
fn add_status_task(fixture: &Fixture, title: &str, status: TaskStatus) -> orbit_types::task::Task {
    add_task(
        fixture,
        TaskAddParams {
            title: title.into(),
            status: Some(status),
            ..Default::default()
        },
    )
}
fn add_task(fixture: &Fixture, params: TaskAddParams) -> orbit_types::task::Task {
    fixture.runtime.add_task(params).unwrap()
}
fn send_ok(server: &Server, method: &str, path: &str, body: Value) -> Value {
    json_ok(server.send(method, path, body))
}
/// Dashboard-only detail extras, stripped before comparing with `orbit.task.show`.
const DETAIL_EXTRAS: &[&str] = &[
    "status_transitions",
    "job_run_navigable",
    "escalation_triage",
    "why_stuck",
    "job_run_worktree",
    "riskiness",
];

fn seed_cli_failure(fixture: &Fixture, id: &str, ts: DateTime<Utc>, blob_ref: &str) {
    let payload = json!({"event_id": id, "ts": ts.to_rfc3339(), "body_kind": "cli_invocation_finished", "run_id": "jrun-friction-fixture", "step_id": "implement", "provider": id, "exit_code": 1, "stderr_blob_ref": blob_ref});
    let params = V2AuditEventInsertParams {
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
        payload_json: payload.to_string(),
    };
    fixture.runtime.insert_v2_audit_event(&params).unwrap();
}

projections_test!(
    friction_stderr_excerpts_bound_bytes_and_tolerate_missing_blobs,
    fixture,
    Fixture::new(),
    {
        // The dashboard API's per-message cap for list projections.
        const CAP: usize = 2048;
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
        let path = "/api/diagnostics/friction?month=2026-04&limit=20&workspace=ws_http_fixture";
        let result = get_ok(&server, path);
        let rows = result.as_array().unwrap();
        check_eq!(rows.len() => cases.len(); "friction row count: {result}");
        for (id, _, expected, truncated) in cases {
            let row = find_row(rows, "command", id);
            check_eq!(row["stderr"] => expected; "bounded stderr for {id}");
            check_eq!(row["truncated"] => truncated; "truncation flag for {id}");
            assert_fields!(row, "step": json!("implement"), "exit_code": json!(1));
        }
    }
);

projections_test!(
    friction_polls_reuse_projection_without_mixing_months_or_limits,
    fixture,
    Fixture::new(),
    {
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
        check!(get_ok(&empty_server, path).as_array().unwrap().is_empty(); "empty fixture has no friction rows");
        let server = fixture.server(false);
        let get = |path: &str| get_ok(&server, path);
        check!(server.origin != empty_server.origin; "servers must bind distinct origins");
        let first = get(path);
        check_eq!(first.as_array().unwrap().len() => 1; "first poll len: {first}");
        assert_fields!(first[0], "command": json!("first"));
        // If a second poll rescans the audit store it will return the new event, rather than the previously computed projection.
        seed_cli_failure(&fixture, "second", ts + Duration::seconds(1), &blob_ref);
        check_eq!(get(path) => first; "poll must reuse its memo");
        let two = get("/api/diagnostics/friction?month=2026-04&limit=2&workspace=ws_http_fixture");
        check_eq!(two.as_array().unwrap().len() => 2; "limit 2 poll len: {two}");
        assert_fields!(two[0], "command": json!("second"));
        assert_fields!(two[1], "command": json!("first"));
        let previous =
            get("/api/diagnostics/friction?month=2026-03&limit=1&workspace=ws_http_fixture");
        check!(previous.as_array().unwrap().is_empty(); "month has its own memo");
        check_eq!(status_of(&server, "/api/diagnostics/friction?month=invalid&workspace=ws_http_fixture") => 400; "invalid month rejected");
    }
);

projections_test!(
    cost_and_metrics_windows_scope_rows_and_timestamp_arithmetic,
    fixture,
    Fixture::new(),
    {
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
        let get = |path: &str| get_ok(&server, path);
        let ws = "workspace=ws_http_fixture";
        for (window, duration, expected) in [
            ("1h", Some(Duration::hours(1)), 1),
            ("24h", Some(Duration::hours(24)), 3),
            ("7d", Some(Duration::days(7)), 7),
            ("30d", Some(Duration::days(30)), 15),
            ("all", None, 31),
        ] {
            let before = Utc::now();
            let result = get(&format!("/api/cost?window={window}&{ws}"));
            let after = Utc::now();
            let metrics = get(&format!("/api/diagnostics/metrics?since={window}&{ws}"));
            check_eq!(metrics.as_array().unwrap().iter().map(|row| row["retry_count"].as_u64().unwrap()).sum::<u64>() => expected; "metrics must filter all month partitions before limiting: {window}");
            assert_fields!(result, "window": json!(window));
            if let Some(duration) = duration {
                let since = parse_ts(&result["window_since"]);
                check!(since >= before - duration && since <= after - duration; "{window} timestamp arithmetic: {since}");
            } else {
                check!(result["window_since"].is_null(); "all window_since null: {result}");
            }
        }
        let limited = get("/api/diagnostics/metrics?since=all&limit=1&workspace=ws_http_fixture");
        check_eq!(limited.as_array().unwrap().len() => 1; "limited metrics len: {limited}");
        assert_fields!(limited[0], "retry_count": json!(1));
        check!(get("/api/diagnostics/metrics?since=all&limit=0&workspace=ws_http_fixture").as_array().unwrap().is_empty(); "limit 0 returns no metrics");
        check_eq!(status_of(&server, "/api/diagnostics/metrics?since=bogus&workspace=ws_http_fixture") => 400; "bogus since rejected");
        check_eq!(status_of(&server, "/api/diagnostics/metrics?since=24h&month=2026-04&workspace=ws_http_fixture") => 400; "since+month rejected");
        assert_fields!(get("/api/cost?workspace=ws_http_fixture"), "window": json!("all"));
        check_eq!(status_of(&server, "/api/cost?window=bogus&workspace=ws_http_fixture") => 400; "bogus cost window rejected");
        // The retired scoreboard route answers with the cost body until
        // its sunset, and says so in the response headers.
        let alias = server.get("/api/scoreboard?window=24h&workspace=ws_http_fixture");
        check_eq!(alias.headers()["deprecation"] => "true"; "scoreboard alias deprecation header");
        check!(alias.headers().contains_key("sunset"); "scoreboard alias sunset header");
        let alias = json_ok(alias);
        assert_fields!(alias, "window": json!("24h"));
        check!(alias["orchestration"].is_object(); "scoreboard alias orchestration: {alias}");
    }
);

/// One invocation row, then moved to `at` (and, for `partition`, to another
/// workspace) the way only the store can. Legacy metrics JSONL stays absent.
fn seed_row(
    fixture: &Fixture,
    run: &str,
    task: &str,
    input: u64,
    output: u64,
    at: DateTime<Utc>,
    partition: Option<&str>,
) {
    let row = (
        "implement",
        "codex",
        Some("fixture-a".into()),
        vec![task.into()],
        "read",
        8,
        input,
        output,
    );
    insert_invocation(&fixture.runtime, run, row);
    place_invocation(fixture, run, at, partition);
}

/// Row: (activity, agent, model, task_ids, tool, result_bytes, input_tokens, output_tokens).
type InvocationRow<'a> = (
    &'a str,
    &'a str,
    Option<String>,
    Vec<String>,
    &'a str,
    u64,
    u64,
    u64,
);
fn insert_invocation(runtime: &orbit_core::OrbitRuntime, run: &str, row: InvocationRow<'_>) {
    let (activity, agent, model, task_ids, tool, result_bytes, input, output) = row;
    runtime
        .insert_invocation_trace_record(&InvocationInsertParams {
            agent_execution_id: None,
            job_run_id: run.into(),
            activity_id: activity.into(),
            agent: agent.into(),
            provider: None,
            account: None,
            model,
            task_ids,
            trace: InvocationTrace {
                usage: TokenUsage {
                    input,
                    output,
                    ..TokenUsage::default()
                },
                tool_calls: vec![ToolCallTrace {
                    seq: 1,
                    tool_name: tool.into(),
                    result_bytes,
                    result_payload: None,
                }],
                duration_ms: 1_000,
                ..InvocationTrace::default()
            },
        })
        .unwrap();
}

/// Move the newest invocation for `run` to `at` (and optionally another workspace).
fn place_invocation(fixture: &Fixture, run: &str, at: DateTime<Utc>, partition: Option<&str>) {
    fixture.runtime.sqlite_store().unwrap().with_transaction(|tx| {
        tx.connection().execute(
            "UPDATE invocations SET ts = ?1, workspace_id = COALESCE(?2, workspace_id)
             WHERE id = (SELECT id FROM invocations WHERE job_run_id = ?3 ORDER BY id DESC LIMIT 1)",
            (at.to_rfc3339(), partition, run),
        ).map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
        Ok(())
    }).unwrap();
}

fn seed_filtered(
    fixture: &Fixture,
    run: &str,
    row: (&str, &str, Option<&str>, &str, &str),
    at: DateTime<Utc>,
    partition: Option<&str>,
) {
    let (activity, agent, model, task, tool) = row;
    let row = (
        activity,
        agent,
        model.map(str::to_string),
        vec![task.into()],
        tool,
        8,
        1,
        1,
    );
    insert_invocation(&fixture.runtime, run, row);
    place_invocation(fixture, run, at, partition);
}

projections_test!(
    diagnostics_metrics_fallback_keeps_foreign_rows_out_of_the_limit,
    fixture,
    Fixture::new(),
    {
        check_eq!(fixture.runtime.workspace_id().unwrap() => "ws_http_fixture"; "fixture workspace id");
        check!(!fixture.runtime.data_root().join("state/diagnostics/metrics").exists(); "legacy metrics must be absent so the invocation fallback runs");
        let now = Utc::now();
        let older = now - Duration::hours(2);
        let newer = now - Duration::hours(1);
        let foreign_at = now - Duration::minutes(1);
        seed_row(&fixture, "a-old", "ORB-A-OLD", 1, 1, older, None);
        seed_row(&fixture, "a-new", "ORB-A", 4, 6, newer, None);
        // Newest row on the shared host store, recorded under another workspace.
        seed_row(
            &fixture,
            "b-foreign",
            "ORB-B",
            100,
            100,
            foreign_at,
            Some("ws_other"),
        );
        let server = fixture.server(false);
        let path = |limit: u32| {
            format!("/api/diagnostics/metrics?since=all&limit={limit}&workspace=ws_http_fixture")
        };
        let limited = get_ok(&server, &path(1));
        let rows = limited.as_array().unwrap();
        check_eq!(rows.len() => 1; "limit applies after the workspace predicate");
        assert_fields!(rows[0], "job_run": json!("a-new"), "task_id": json!("ORB-A"), "step": json!("implement"), "actor_identity": json!("codex / fixture-a"), "token_usage": json!(10), "tool_invocations": json!(1), "step_duration_ms": json!(1_000), "retry_count": json!(0), "ts": json!(newer.to_rfc3339()));
        let both = get_ok(&server, &path(2));
        let runs: Vec<_> = both
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["job_run"].as_str().unwrap())
            .collect();
        check_eq!(runs => ["a-new", "a-old"]; "a newer foreign row must not displace either workspace row");
        let foreign_left = fixture
            .runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "DELETE FROM invocations WHERE workspace_id = ?1",
                        ["ws_http_fixture"],
                    )
                    .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                let count: i64 = tx
                    .connection()
                    .query_row(
                        "SELECT COUNT(*) FROM invocations WHERE workspace_id = 'ws_other'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
                Ok(count)
            })
            .unwrap();
        check_eq!(foreign_left => 1; "the other workspace still has history in the shared store");
        let empty = get_ok(&server, &path(1));
        check!(empty.as_array().unwrap().is_empty(); "an invocation-empty workspace returns [] while another workspace still has history");
    }
);

projections_test!(
    invocation_metrics_scope_workspace_before_filters_and_limit,
    fixture,
    Fixture::new(),
    {
        check_eq!(fixture.runtime.workspace_id().unwrap() => "ws_http_fixture"; "fixture workspace id");
        let now = Utc::now();
        let selected_at = now - Duration::minutes(2);
        let foreign_at = now - Duration::minutes(1);
        let run = "shared-run";
        // Every distractor misses exactly one filter; the last row is a newer foreign-workspace match that must not consume the limit.
        let act = "selected-activity";
        let ag = "selected-agent";
        let mo = Some("selected-model");
        let ta = "ORB-SELECTED";
        let to = "selected-tool";
        let oa = "other-activity";
        let og = "other-agent";
        let om = Some("other-model");
        seed_filtered(&fixture, run, (act, ag, mo, ta, to), selected_at, None);
        seed_filtered(&fixture, run, (oa, ag, mo, ta, to), selected_at, None);
        seed_filtered(&fixture, run, (act, og, mo, ta, to), selected_at, None);
        seed_filtered(&fixture, run, (act, ag, om, ta, to), selected_at, None);
        seed_filtered(
            &fixture,
            run,
            (act, ag, mo, "ORB-OTHER", to),
            selected_at,
            None,
        );
        seed_filtered(
            &fixture,
            run,
            (act, ag, mo, ta, "other-tool"),
            selected_at,
            None,
        );
        let old_at = now - Duration::minutes(10);
        seed_filtered(&fixture, run, (act, ag, mo, ta, to), old_at, None);
        seed_filtered(
            &fixture,
            run,
            (act, ag, mo, ta, to),
            foreign_at,
            Some("ws_other"),
        );
        let server = fixture.server(false);
        let get = |path: &str| get_ok(&server, path);
        let unfiltered = get("/api/metrics/invocations?limit=1&workspace=ws_http_fixture");
        let unfiltered_rows = unfiltered.as_array().unwrap();
        check_eq!(unfiltered_rows.len() => 1; "unfiltered limit: {unfiltered}");
        assert_fields!(unfiltered_rows[0], "job_run_id": json!(run));
        check_eq!(parse_ts(&unfiltered_rows[0]["ts"]) => selected_at; "unfiltered row timestamp");
        let since = (now - Duration::minutes(3))
            .to_rfc3339()
            .replace("+00:00", "Z");
        let until = (now - Duration::seconds(30))
            .to_rfc3339()
            .replace("+00:00", "Z");
        let ws = "workspace=ws_http_fixture";
        let query = format!(
            "/api/metrics/invocations?job_run_id={run}&activity_id=selected-activity&agent=selected-agent&model=selected-model&task_id=ORB-SELECTED&tool_name=selected-tool&since={since}&until={until}&limit=20&{ws}"
        );
        let filtered = get(&query);
        let filtered_rows = filtered.as_array().unwrap();
        check_eq!(filtered_rows.len() => 1; "all filters compose with workspace selection: {filtered}");
        assert_fields!(filtered_rows[0], "job_run_id": json!(run), "activity_id": json!("selected-activity"), "agent": json!("selected-agent"), "model": json!("selected-model"), "task_ids": json!(["ORB-SELECTED"]));
        check_eq!(filtered_rows[0]["tool_calls"][0]["tool_name"] => "selected-tool"; "filtered tool call");
        check_eq!(parse_ts(&filtered_rows[0]["ts"]) => selected_at; "filtered row timestamp");
    }
);

projections_test!(
    duration_overflow_is_a_client_error_and_server_remains_responsive,
    fixture,
    Fixture::new(),
    {
        fs::write(fixture.path("process.log"), "").unwrap();
        let server = fixture.server(false);
        let get = |path: &str| get_ok(&server, path);
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
                check_eq!(response.status().as_u16() => 400; "overflow {duration} on {endpoint}");
                check!(response.json::<Value>().unwrap()["error"].is_string(); "overflow error body for {duration} on {endpoint}");
            }
        }
        check_eq!(status_of(&server, "/healthz") => 200; "server remains responsive");
        for duration in ["30m", "2h", "1d", "2w", "120"] {
            get(&format!(
                "/api/audit/summary?since={duration}&workspace=ws_http_fixture"
            ));
        }
    }
);

projections_test!(
    auto_task_and_routine_schedules_distinguish_armed_and_hypothetical_times,
    fixture,
    Fixture::new(),
    {
        for (name, schedule) in [
            ("interval", AutoTaskSchedule::every_minutes(60)),
            ("cron", AutoTaskSchedule::cron("* * * * *")),
            ("disabled", AutoTaskSchedule::every_minutes(60)),
            ("unobserved", AutoTaskSchedule::every_minutes(60)),
        ] {
            seed_schedule_task(&fixture, name, schedule);
        }
        fixture.runtime.auto_task_toggle("disabled", false).unwrap();
        let baseline = Utc::now()
            .with_second(0)
            .unwrap()
            .with_nanosecond(0)
            .unwrap()
            - Duration::minutes(10);
        let cursor = cursor_state_path(&fixture.runtime.paths().state_dir);
        let cursor_json = json!({"definitions":{
            "interval":{"baseline_at":baseline.to_rfc3339()},
            "cron":{"baseline_at":baseline.to_rfc3339()},
            "disabled":{"baseline_at":baseline.to_rfc3339()},
        }});
        write_json(&cursor, cursor_json);
        fixture.job("schedule_fixture");
        let routines = fixture.work.join("routines");
        fs::create_dir_all(&routines).unwrap();
        for (name, enabled) in [("armed", true), ("disabled", false), ("paused", true)] {
            let yaml = format!(
                "schemaVersion: 1\nname: {name}\ndescription: HTTP projection\nenabled: {enabled}\ntrigger: {{cron: '* * * * *'}}\ntarget: job:schedule_fixture\n"
            );
            fs::write(routines.join(format!("{name}.yaml")), yaml).unwrap();
        }
        pause_routine(&fixture.global, "paused", "http-fixture").unwrap();
        let server = fixture.server(false);
        let before = Utc::now();
        let auto = get_ok(&server, "/api/auto-tasks?workspace=ws_http_fixture");
        let after = Utc::now();
        // Cron is evaluated in host-local time, so both payloads name that
        // zone for the dashboard to label triggers with.
        let host_offset = chrono::Local::now().offset().local_minus_utc();
        check_eq!(auto["cron_zone"]["offset_seconds"] => host_offset; "cron zone offset: {auto}");
        check!(auto["cron_zone"]["name"].is_string() || auto["cron_zone"]["name"].is_null(); "cron zone name: {auto}");
        let definitions = auto["definitions"].as_array().unwrap();
        check_eq!(definitions.len() => 4; "auto-task definitions: {auto}");
        for (name, state, hypothetical) in [
            ("interval", "scheduled", false),
            ("cron", "scheduled", false),
            ("disabled", "disabled", true),
            ("unobserved", "never_observed", false),
        ] {
            let row = find_row(definitions, "name", name);
            assert_next(row, state, hypothetical);
            if name == "unobserved" {
                check!(row["next_evaluation"]["at"].is_null(); "unobserved at for {name}: {row}");
            } else if name == "cron" {
                let at = parse_ts(&row["next_evaluation"]["at"]);
                check!(at > before && at <= after + Duration::minutes(1); "next cron minute for {name}: {at}");
                check_eq!(at.second() => 0; "next cron minute seconds for {name}");
            } else {
                check_eq!(parse_ts(&row["next_evaluation"]["at"]) => baseline + Duration::minutes(60); "interval retains first-observed anchor for {name}");
            }
        }
        let before = Utc::now();
        let report = get_ok(&server, "/api/routines?workspace=ws_http_fixture");
        let after = Utc::now();
        check_eq!(report["cron_zone"] => auto["cron_zone"]; "routines cron zone: {report}");
        let rows = report["routines"].as_array().unwrap();
        check_eq!(rows.len() => 3; "routine rows: {report}");
        for (name, state, hypothetical) in [
            ("armed", "scheduled", false),
            ("disabled", "disabled", true),
            ("paused", "paused", true),
        ] {
            let row = find_row(rows, "name", name);
            assert_next(row, state, hypothetical);
            let at = parse_ts(&row["next_evaluation"]["at"]);
            check!(at > before && at <= after + Duration::minutes(1); "routine next cron minute for {name}: {at}");
            check_eq!(at.second() => 0; "routine next cron seconds for {name}");
        }
        // A corrupt host cursor is unavailable, not silently treated as never observed.
        fs::write(&cursor, "{broken").unwrap();
        let bad = get_ok(&server, "/api/auto-tasks?workspace=ws_http_fixture");
        check!(bad["cursor_state_error"].is_string(); "cursor error reported: {bad}");
        let interval = find_row(bad["definitions"].as_array().unwrap(), "name", "interval");
        assert_fields!(interval["next_evaluation"], "state": json!("unavailable"), "at": Value::Null);
        check_eq!(fs::read_to_string(cursor).unwrap() => "{broken"; "GET must preserve corrupt evidence");
    }
);

// [ORB-14173] Replica: GC routine toggleable, owner's work apart and named;
// every refused toggle is refused without a write, each under its own code.
projections_test!(
    replica_routines_project_only_worktree_gc_as_toggleable,
    fixture,
    Fixture::replica_of("hm_fixture_remote"),
    {
        fixture.job("worktree_gc_pipeline");
        fixture.job("ci_failure_sweep_pipeline");
        let routines = fixture.work.join("routines");
        fs::create_dir_all(&routines).unwrap();
        for (name, job) in [
            ("replica-gc", "worktree_gc_pipeline"),
            ("replica-ci-sweep", "ci_failure_sweep_pipeline"),
        ] {
            fs::write(
                    routines.join(format!("{name}.yaml")),
                    format!(
                        "schemaVersion: 1\nname: {name}\nenabled: false\ntrigger: {{cron: '* * * * *'}}\ntarget: job:{job}\n"
                    ),
                )
                .unwrap();
        }
        let ci_sweep_path = routines.join("replica-ci-sweep.yaml");
        let ci_sweep_before = fs::read(&ci_sweep_path).unwrap();
        let server = fixture.server(true);
        let report = get_ok(&server, "/api/routines?workspace=ws_http_fixture");
        let scheduled = report["routines"].as_array().unwrap();
        check_eq!(scheduled.len() => 1; "scheduled routines: {report}");
        assert_fields!(scheduled[0], "name": json!("replica-gc"), "origin": json!("workspace"));
        let owner_only = report["owner_only"].as_array().unwrap();
        check_eq!(owner_only.len() => 1; "owner-only routines: {report}");
        assert_fields!(owner_only[0], "name": json!("replica-ci-sweep"), "owner_machine": json!("hm_fixture_remote"));
        check_eq!(fs::canonicalize(owner_only[0]["path"].as_str().unwrap()).unwrap() => fs::canonicalize(&ci_sweep_path).unwrap(); "owner-only path: {report}");
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
        let gc_job = "job:worktree_gc_pipeline";
        let ci_job = "job:ci_failure_sweep_pipeline";
        // Each refused toggle must leave the GC routine file untouched.
        error_code(toggle("missing", gc_job), 404, "routine_not_found");
        check_eq!(fs::read(&gc_path).unwrap() => gc_before; "routine_not_found wrote");
        error_code(
            toggle_from("replica-gc", "elsewhere", gc_job, false),
            409,
            "workspace_mismatch",
        );
        check_eq!(fs::read(&gc_path).unwrap() => gc_before; "workspace_mismatch wrote");
        error_code(toggle("replica-gc", ci_job), 409, "target_mismatch");
        check_eq!(fs::read(&gc_path).unwrap() => gc_before; "target_mismatch wrote");
        error_code(
            toggle_from("replica-gc", "fixture", gc_job, true),
            409,
            "stale_routine_state",
        );
        check_eq!(fs::read(&gc_path).unwrap() => gc_before; "stale_routine_state wrote");
        let refused = error_code(toggle("replica-ci-sweep", ci_job), 409, "owner_authority");
        assert_contains(&refused, "error", "hm_fixture_remote");
        check_eq!(fs::read(&ci_sweep_path).unwrap() => ci_sweep_before; "owner toggle wrote");
        let enabled = json_ok(toggle("replica-gc", gc_job));
        assert_fields!(enabled, "changed": json!(true));
        check!(fs::read_to_string(&gc_path).unwrap().contains("enabled: true"); "GC routine enabled on disk");
    }
);

// [ORB-14173] Replica auto-task controls must match the scheduler's owner-only
// boundary: the dashboard names the owner and neither toggle nor manual mint
// mutates replica control-plane state.
projections_test!(
    replica_auto_task_controls_explain_owner_authority_and_refuse_mutations,
    fixture,
    Fixture::new(),
    {
        let text = (
            "Should remain unminted".into(),
            "Replica control boundary fixture",
            "An auto-task may not issue work from a replica.",
            vec!["No task is minted.".into()],
        );
        seed_auto_task(
            &fixture,
            "qa-sweep",
            AutoTaskSchedule::every_minutes(60),
            text,
        );
        let fixture = fixture.into_replica("hm_fixture_remote");
        let definition_path = fixture.work.join("auto_tasks/qa-sweep.yaml");
        let definition_before = fs::read(&definition_path).unwrap();
        let server = fixture.server(true);
        let listed = get_ok(&server, "/api/auto-tasks?workspace=ws_http_fixture");
        assert_fields!(listed, "controls_authorized": json!(false));
        for action in ["auto_task_toggle", "auto_task_mint", "auto_task_delete"] {
            let capability = &listed["capabilities"][action];
            assert_fields!(capability, "authorized": json!(false));
            assert_contains(capability, "reason", "hm_fixture_remote");
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
        assert_contains(&refused_toggle, "error", "hm_fixture_remote");
        let refused_mint = error_code(
            server.send(
                "POST",
                "/api/auto-tasks/mint?workspace=ws_http_fixture",
                json!({"name":"qa-sweep","acknowledge_unconditional":true}),
            ),
            403,
            "capability_refused",
        );
        assert_contains(&refused_mint, "error", "hm_fixture_remote");
        let refused_delete = error_code(
            server.send(
                "POST",
                "/api/auto-tasks/delete?workspace=ws_http_fixture",
                json!({"name":"qa-sweep","reason":"replica"}),
            ),
            403,
            "capability_refused",
        );
        assert_contains(&refused_delete, "error", "hm_fixture_remote");
        check_eq!(fs::read(&definition_path).unwrap() => definition_before; "replica mutations wrote definition");
        check!(fixture.runtime.list_tasks_by_tags(&["auto-task:qa-sweep".into()]).unwrap().is_empty(); "replica mint created a task");
    }
);

#[path = "../../../orbit-store/tests/fixtures/policy_denials.rs"]
mod policy_denials_fixture;

projections_test!(
    policy_kpi_counts_decisions_and_preserves_refusal_evidence,
    fixture,
    Fixture::new(),
    {
        use policy_denials_fixture::{RAW_DENIED_COUNT, SQL_POLICY_COUNT, V2_POLICY_COUNT};
        policy_denials_fixture::seed(&fixture.runtime);
        let stats = fixture.runtime.audit_policy_denial_stats(None).unwrap();
        check_eq!(stats.sql_denied => SQL_POLICY_COUNT; "SQL denial count");
        check_eq!(stats.v2_denied => V2_POLICY_COUNT; "V2 denial count");
        let counts: std::collections::BTreeMap<_, _> = stats.by_operation.into_iter().collect();
        let c = &counts;
        count_eq(
            c,
            "orbit.workflow.run.show",
            1,
            "legacy authorization/tool pair is one attempt",
        );
        count_eq(
            c,
            "orbit.command.exec",
            2,
            "MCP request IDs are scoped to sessions",
        );
        count_eq(
            c,
            "orbit.task.locks.reserve",
            1,
            "only the capability refusal counts, not contention",
        );
        count_eq(
            c,
            "orbit.drain.claim.settle",
            1,
            "only the capability refusal counts, not owner configuration",
        );
        count_eq(c, "fs.read", 1, "fs.read denial count");
        count_eq(c, "proc.spawn", 1, "proc.spawn denial count");
        let server = fixture.server(false);
        let get = |path: &str| get_ok(&server, path);
        let summary = get("/api/audit/summary?since=24h&workspace=ws_http_fixture");
        assert_fields!(summary, "denials_sql": json!(SQL_POLICY_COUNT), "denials_v2": json!(V2_POLICY_COUNT), "denials": json!(SQL_POLICY_COUNT + V2_POLICY_COUNT));
        let policy = get("/api/diagnostics/denials?since=24h&workspace=ws_http_fixture");
        check_eq!(policy["total"] => RAW_DENIED_COUNT + V2_POLICY_COUNT; "policy total: {policy}");
        check_eq!(policy["policy_decisions"]["total"] => summary["denials"]; "policy decisions total: {policy}");
        assert_fields!(policy["policy_decisions"], "sql": json!(SQL_POLICY_COUNT), "v2": json!(V2_POLICY_COUNT));
        let tool_policy =
            get("/api/diagnostics/denials?kind=tool&since=24h&workspace=ws_http_fixture");
        let recent = tool_policy["recent_denials"].as_array().unwrap();
        check_eq!(tool_policy["policy_decisions"] => policy["policy_decisions"]; "evidence filters must not change the top-bar population");
        check!(tool_policy["total"].as_i64().unwrap() < policy["total"].as_i64().unwrap(); "tool total below policy total: {tool_policy} vs {policy}");
        check!(recent.iter().any(|row| row["denial_kind"] == "claim_settlement_refusal"); "claim settlement refusal in {recent:?}");
        check!(recent.iter().any(|row| row["denial_kind"] == "task_lock_reserve"); "task lock reserve in {recent:?}");
        let raw = get("/api/audit?status=denied&workspace=ws_http_fixture");
        check_eq!(raw.as_array().unwrap().len() => RAW_DENIED_COUNT as usize; "raw denied count: {raw}");
    }
);

projections_test!(
    dashboard_memo_polls_report_payload_age,
    fixture,
    Fixture::new(),
    {
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
            check!(first_age.parse::<u64>().is_ok(); "{path}: Age {first_age:?}");
            check!(repeat_age.parse::<u64>().is_ok(); "{path}: Age {repeat_age:?}");
            check_eq!(json_ok(repeat) => first; "{path}: a repeat poll inside the TTL is served from the memo");
        }
    }
);

// A `github-pr` ref recorded with only its number links to the pull request
// in the workspace's own GitHub repository, in the detail and on list rows.
// A recorded page is kept; a ref whose id is not a PR number, or a workspace
// whose origin is not on GitHub, stays unlinked.
projections_test!(
    github_pr_refs_link_the_pull_request_from_the_origin_remote,
    fixture,
    Fixture::new(),
    {
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
            send_ok(&server, "POST", "/api/tasks?workspace=ws_http_fixture", json!({"title": "PR link fixture", "description": "Delivered work", "acceptance_criteria": ["Link the PR"], "complexity": "low", "external_refs": refs}))["id"].as_str().unwrap().to_string()
        };
        let refs = |id: &str| task_detail(&server, id)["external_refs"].clone();
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
        check_eq!(refs(&number_only) => linked; "number-only ref links from origin");
        check_eq!(refs(&recorded) => json!([{"system": "github-pr", "id": "12", "url": "https://github.example/o/r/pull/12"}]); "recorded page is kept");
        check_eq!(refs(&not_a_number) => json!([{"system": "github-pr", "id": "40/../67"}]); "non-number id stays unlinked");
        let page = task_page(&server);
        check_eq!(row_of(&page, &number_only)["external_refs"] => linked; "the list row links it too");
        git(&[
            "remote",
            "set-url",
            "origin",
            "https://gitlab.example/o/r.git",
        ]);
        check_eq!(refs(&number_only) => json!([{"system": "github-pr", "id": "4067"}, {"system": "jira", "id": "ENG-1"}]); "a remote off GitHub leaves the number unlinked");
    }
);

// Detail = shared show record plus dashboard extras (`status_transitions`,
// `job_run_navigable`, `readiness.preparing`, held `escalation_triage`);
// an unconfigured crew reads as `crew_unresolved`, not the default crew.
projections_test!(
    task_detail_and_list_rows_serve_the_shared_task_projection,
    fixture,
    Fixture::new(),
    {
        let server = fixture.server(true);
        let create = |server: &Server, body: Value| {
            send_ok(server, "POST", "/api/tasks?workspace=ws_http_fixture", body)["id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        let body = json!({"title": "Projection parity", "description": "Shared JSON", "acceptance_criteria": ["Match"], "complexity": "low"});
        let id = create(&server, body);
        let source = fixture.runtime.paths().repo_root.join("parity.txt");
        fs::write(&source, "parity artifact\n").unwrap();
        fixture.runtime.run_tool("orbit.task.artifact.put", json!({"id": id, "source_path": source, "path": "reports/parity.txt", "model": "codex"})).unwrap();
        let full_detail = task_detail(&server, &id);
        let mut detail = full_detail.clone();
        strip(
            &mut detail,
            &["status_transitions", "job_run_navigable", "riskiness"],
        );
        check!(detail["readiness"].as_object_mut().unwrap().remove("preparing").is_some(); "dashboard extra `readiness.preparing`");
        check_eq!(detail => show_task(&fixture, &id); "the detail is the shared task record");
        check!(detail.as_object().unwrap().contains_key("job_run_machine"); "detail job_run_machine: {detail}");
        assert_fields!(detail["artifacts"][0], "size_bytes": json!(16));
        let page = task_page(&server);
        check!(page.get("detail").is_none(); "ordinary task pages keep their existing shape");
        let row = row_of(&page, &id).clone();
        let listed = listed_tasks(&fixture);
        let tool_row = tool_row(&listed, &id);
        for (key, value) in tool_row.as_object().unwrap() {
            match key.as_str() {
                "description" | "plan" | "execution_summary" | "acceptance_criteria" => {
                    assert!(row.get(key).is_none(), "a summary row omits `{key}`");
                }
                "readiness" => assert_eq!(row[key]["gaps"], value["gaps"]),
                _ => assert_eq!(&row[key], value, "list row `{key}`"),
            }
        }
        check!(row.as_object().unwrap().contains_key("job_run_machine"); "list row job_run_machine: {row}");
        let bundled = get_ok(
            &server,
            &format!("/api/tasks?workspace=ws_http_fixture&detail={id}&detail_runs=true"),
        );
        check_eq!(bundled["detail"] => full_detail; "the optional list projection equals task detail");
        check!(bundled["detail_runs"]["items"].as_array().unwrap().is_empty(); "detail_runs empty: {bundled}");
        drop(server);
        // Author a task while its crew exists, then serve it from a host
        // configuration that no longer defines that crew.
        let global_path = fixture.global.join("config.toml");
        let baseline = fs::read_to_string(&global_path).unwrap();
        let configured = concat!(
            "\n[workflow]\ndefault_crew = \"keep\"\n",
            "\n[crews.keep]\nprovider = \"claude\"\nmodel = \"keep-model\"\n",
        );
        fs::write(&global_path, format!("{baseline}{configured}\n[crews.gone]\nprovider = \"claude\"\nmodel = \"gone-model\"\n")).unwrap();
        let server = fixture.server(true);
        let stale_body = json!({"title": "Stale crew", "description": "Crew removed later", "acceptance_criteria": ["Stay readable"], "complexity": "low", "crew": "gone"});
        let stale = create(&server, stale_body);
        drop(server);
        fs::write(&global_path, format!("{baseline}{configured}")).unwrap();
        let server = fixture.server(true);
        let detail = task_detail(&server, &stale);
        assert_fields!(detail, "crew": json!("gone"));
        check!(detail.get("resolved_crew").is_none() && detail.get("crew_model").is_none(); "an unresolvable crew is not swapped for the default: {detail}");
        check!(detail["crew_unresolved"].as_str().is_some_and(|reason| !reason.is_empty()); "the detail names why the crew is unresolved: {detail}");
    }
);

/// A run-blocked task with an escalation recorded without another status change.
fn block_with_final_recovery(
    runtime: &orbit_core::OrbitRuntime,
    repo: &Path,
    id: &str,
    run_id: &str,
) {
    let failure = blocked_workflow_failure_update(
        "task_pr_pipeline",
        run_id,
        Some("STEP_FAILED"),
        Some("step `implement` completed with success=false"),
    );
    let update = TaskAutomationUpdate {
        job_run_id: Some(run_id.into()),
        ..failure
    };
    runtime.apply_task_automation_update(id, update).unwrap();
    let task = runtime.get_task(id).unwrap();
    let observed = runtime.final_recovery_revision(&task).unwrap();
    let request = FinalRecoveryRequest {
        task_id: id.to_string(),
        run_id: run_id.to_string(),
        observed,
        repo_root: repo.to_path_buf(),
        base_ref: "main".into(),
        completion: FinalRecoveryCompletion::Review,
        requeue_bound: Default::default(),
    };
    let decision = json!({"decision": "escalate", "diagnosis": "the failure needs an operator", "human_action": "move the task back to backlog after fixing the cause"});
    let outcome = runtime
        .apply_final_recovery(&request, Some(&decision))
        .unwrap();
    check_eq!(outcome => FinalRecoveryOutcome::Escalated { reason: None }; "final recovery outcome for {id}");
}

// Blocked detail matches `orbit.task.show` plus dashboard extras; list rows
// and mutation responses omit show-only guidance and the triage readout.
projections_test!(
    task_detail_includes_blocked_guidance_and_list_rows_omit_it,
    fixture,
    Fixture::new(),
    {
        let blocked = add_titled_task(&fixture, "Blocked guidance", "Show the next step");
        let plain = add_titled_task(&fixture, "Plain detail", "Stay quiet");
        let run_id = "jrun-http-guidance";
        block_with_final_recovery(&fixture.runtime, &fixture.path("repo"), &blocked, run_id);
        let server = fixture.server(true);
        let raw = task_detail(&server, &blocked);
        check!(raw.get("status_transitions").is_some(); "status_transitions extra: {raw}");
        check!(raw.get("job_run_navigable").is_some(); "job_run_navigable extra: {raw}");
        check_eq!(raw["next_step"] => json!({"run_id": run_id, "resume_command": null, "requeue_command": format!("orbit task update {blocked} --status backlog")}); "blocked next_step");
        assert_fields!(raw["final_recovery"], "run_id": json!(run_id), "decision": json!("escalate"), "outcome": json!("escalated"));
        assert_fields!(raw["block"], "reason": json!("the failure needs an operator"), "run_id": json!(run_id), "next_action": json!("move the task back to backlog after fixing the cause"));
        check_eq!(raw["block"]["at"] => raw["final_recovery"]["at"]; "block at matches recovery at");
        check!(raw["final_recovery"]["at"].as_str().unwrap().contains('T'); "recovery timestamp: {raw}");
        check!(raw["escalation_triage"].is_null(); "a blocked task is decided by final recovery, so it has no triage readout: {raw}");
        let mut detail = raw.clone();
        // A blocked task has no triage readout: final recovery decides it.
        let extras: Vec<&str> = DETAIL_EXTRAS
            .iter()
            .copied()
            .filter(|key| *key != "escalation_triage")
            .collect();
        strip(&mut detail, &extras);
        if let Some(readiness) = detail.get_mut("readiness").and_then(Value::as_object_mut) {
            readiness.remove("preparing");
        }
        let shown = show_task(&fixture, &blocked);
        check_eq!(detail => shown; "the detail is the shared show record");
        no_keys(&shown, &DETAIL_EXTRAS[..5], "the tool show");
        let page = task_page(&server);
        for id in [&blocked, &plain] {
            no_keys(
                row_of(&page, id),
                &["next_step", "final_recovery", "escalation_triage"],
                "a list row",
            );
        }
        no_keys(
            tool_row(&listed_tasks(&fixture), &blocked),
            &["next_step", "final_recovery"],
            "orbit.task.list",
        );
        let blocked_path = format!("/api/tasks/{blocked}?workspace=ws_http_fixture");
        let updated = send_ok(
            &server,
            "PATCH",
            &blocked_path,
            json!({"comment": "operator looked at the block"}),
        );
        check!(updated.get("status_transitions").is_some(); "mutation status_transitions: {updated}");
        no_keys(
            &updated,
            &["next_step", "final_recovery", "escalation_triage"],
            "a mutation response",
        );
        let again = task_detail(&server, &blocked);
        check!(again.get("next_step").is_some() && again.get("final_recovery").is_some(); "detail after comment keeps guidance: {again}");
        check!(again["escalation_triage"].is_null(); "a blocked task keeps no triage readout after a comment: {again}");
        no_keys(
            &task_detail(&server, &plain),
            &["next_step", "final_recovery", "block", "escalation_triage"],
            "plain detail",
        );
    }
);

// Pilot warnings belong to the durable task payload even without a drain.
// Advisory findings are excluded; a clean pilot or leaving proposed clears them.
projections_test!(
    task_payloads_expose_durable_holding_pilot_warnings,
    fixture,
    Fixture::host("orbit", "ORB"),
    {
        let params = TaskAddParams {
            title: "Pilot warning fixture".into(),
            complexity: TaskComplexity::Low,
            tags: vec!["no-diff-expected".into()],
            description: "Durable findings".into(),
            acceptance_criteria: vec!["Expose full findings".into()],
            ..Default::default()
        };
        let id = add_task(&fixture, params).id;
        // Seed the typed automation event paired with each pilot receipt.
        let record = |operation: char, assessment: Value| {
            let decision = (operation != 'b').then(|| TaskDecision::DrainApprovalHeld {
                classification: "warnings".into(),
            });
            record_pilot_receipt(&fixture, &id, operation, assessment, decision);
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
        let assert_payloads =
            |expected: &Value| assert_pilot_warnings(&fixture, &server, &id, expected);
        assert_payloads(&expected);
        add_task(
            &fixture,
            TaskAddParams {
                title: "Newer clean task".into(),
                ..Default::default()
            },
        );
        // Warning filtering happens before pagination and keeps accurate totals.
        let get = |path: &str| get_ok(&server, path);
        for endpoint in ["/api/tasks", "/api/tasks/all"] {
            let filtered = get(&format!(
                "{endpoint}?workspace=ws_http_fixture&pilot=warnings&limit=1"
            ));
            assert_fields!(filtered, "total": json!(1));
            check_eq!(filtered["items"][0]["id"] => id; "filtered row for {endpoint}");
            check!(filtered["next_cursor"].is_null(); "filtered next_cursor: {filtered}");
        }
        let job = "workspace_auto_pipeline";
        let mut drain = fixture.seed_run("jrun-pilot-warning-view", job, JobRunState::Running);
        drain.input = Some(json!({"approve_proposed": true, "max_active_leaf_runs": 4}));
        fixture.save_run(&drain);
        let state = PipelineState::new(
            drain.run_id.clone(),
            drain.job_id.clone(),
            drain.input.clone().unwrap(),
        );
        let rt = &fixture.runtime;
        rt.write_run_state(&drain.run_id, &state).unwrap();
        let readiness = rt.workspace_auto_readiness(&[], None, 500, &[]).unwrap();
        let held_rows = readiness["approvals"]["held"].as_array().unwrap();
        let held = find_row(held_rows, "task_id", &id);
        check_eq!(held["findings"] => expected; "CLI readiness reads the task's latest pilot source");
        let card = get("/api/workflows/auto/readiness?workspace=ws_http_fixture");
        assert_fields!(card["approvals"]["held_by_reason"], "warnings": json!(1));
        let held_rows = card["approvals"]["held"].as_array().unwrap();
        let held = find_row(held_rows, "task_id", &id);
        check!(held.get("findings").is_none(); "the card's HTTP payload needs no finding prose");
        // Ordinary operator comments do not erase the latest verification.
        send_ok(
            &server,
            "PATCH",
            &format!("/api/tasks/{id}?workspace=ws_http_fixture"),
            json!({"comment": "Investigating"}),
        );
        assert_payloads(&expected);
        record(
            'b',
            json!({"utility_warnings": [], "surface_warnings": [], "validation_tool_warnings": []}),
        );
        assert_payloads(&json!([]));
        record('c', assessment);
        assert_payloads(&expected);
        send_ok(
            &server,
            "POST",
            &format!("/api/tasks/{id}/approve?workspace=ws_http_fixture"),
            json!({}),
        );
        assert_payloads(&json!([]));
        // Non-dependency prerequisite tasks:
        // A finished prerequisite holds nothing and clears pilot warnings on every read surface.
        let done_prereq = add_status_task(&fixture, "Done prerequisite", TaskStatus::Done);
        let unfin_prereq =
            add_status_task(&fixture, "Unfinished prerequisite", TaskStatus::Backlog);
        let prereq_task = add_task(
            &fixture,
            TaskAddParams {
                title: "Prerequisite check task".into(),
                complexity: TaskComplexity::Low,
                description: "Prerequisite verification".into(),
                acceptance_criteria: vec!["Acceptance".into()],
                ..Default::default()
            },
        )
        .id;
        let record_prereq = |operation: char, assessment: Value| {
            let decision = Some(TaskDecision::DrainApprovalHeld {
                classification: "warnings".into(),
            });
            record_pilot_receipt(&fixture, &prereq_task, operation, assessment, decision);
        };
        let assert_prereq_payloads =
            |expected: &Value| assert_pilot_warnings(&fixture, &server, &prereq_task, expected);
        // A done non-dependency clears; missing/unfinished non-dependencies still report.
        let done_text = format!("Wait for {} to land", done_prereq.id);
        record_prereq('d', json!({"blocked_by": [done_text]}));
        assert_prereq_payloads(&json!([]));
        record_prereq('e', json!({"blocked_by": ["Wait for ORB-99999 to land"]}));
        let missing = json!([{"kind": "blocked_by", "text": "Wait for ORB-99999 to land"}]);
        assert_prereq_payloads(&missing);
        let unfin_text = format!("Wait for {} to land", unfin_prereq.id);
        record_prereq('f', json!({"blocked_by": [unfin_text.clone()]}));
        assert_prereq_payloads(&json!([{"kind": "blocked_by", "text": unfin_text}]));
    }
);

fn add_scope_workspace(fixture: &Fixture, id: &str, name: &str) -> orbit_core::OrbitRuntime {
    let path = orbit_registry::workspace_registry::registry_path_for(&fixture.global);
    let mut registry = orbit_registry::workspace_registry::load_registry_from(&path).unwrap();
    let mut workspace = registry.workspaces[0].clone();
    workspace.id = id.into();
    workspace.name = name.into();
    let repo = fixture.path(name);
    let work = repo.join(".orbit");
    fs::create_dir_all(&work).unwrap();
    fs::write(
        work.join("config.yaml"),
        format!("schema_version: 1\nworkspace_id: {id}\n"),
    )
    .unwrap();
    let checkout = orbit_types::workspace::WorkspaceCheckout::owner(id.into(), repo, work);
    registry.workspaces.push(workspace.clone());
    registry.checkouts.push(checkout.clone());
    orbit_registry::workspace_registry::save_registry_to(&registry, &path).unwrap();
    RegisteredRuntimeFactory::open_registered_checkout(&fixture.global, &workspace, &checkout)
        .unwrap()
}

#[allow(clippy::too_many_arguments)]
fn seed_scope(
    runtime: &orbit_core::OrbitRuntime,
    run: &str,
    activity: &str,
    agent: &str,
    model: &str,
    tool: &str,
    input: u64,
    output: u64,
    bytes: u64,
) {
    let row = (
        activity,
        agent,
        Some(model.into()),
        Vec::new(),
        tool,
        bytes,
        input,
        output,
    );
    insert_invocation(runtime, run, row);
}

fn null_scope_workspace(fixture: &Fixture, run: &str) {
    fixture
        .runtime
        .sqlite_store()
        .unwrap()
        .with_transaction(|tx| {
            tx.connection()
                .execute(
                    "UPDATE invocations SET workspace_id = NULL WHERE job_run_id = ?1",
                    [run],
                )
                .map_err(|error| orbit_core::OrbitError::Store(error.to_string()))?;
            Ok(())
        })
        .unwrap();
}

// [ORB-20152] Aggregates count only the selected workspace partition,
// with preserved grouping, totals and ordering.
projections_test!(
    activity_and_tool_aggregates_scope_to_selected_workspace_partition,
    fixture,
    Fixture::new(),
    {
        let other = add_scope_workspace(&fixture, "ws_http_scope_other", "scope_other");
        // Each population fixes its agent/model/tool; calls vary only run/activity/tokens.
        let rt = &fixture.runtime;
        let (aa, ma, ta) = ("agent-a", "model-a", "scope-tool-a");
        let (ab, mb, tb) = ("agent-b", "model-b", "scope-tool-b");
        seed_scope(rt, "scope-a-1", "scope-alpha", aa, ma, ta, 10, 20, 12);
        seed_scope(rt, "scope-a-2", "scope-alpha", aa, ma, ta, 10, 20, 12);
        seed_scope(rt, "scope-a-low", "scope-alpha-low", aa, ma, ta, 1, 1, 12);
        seed_scope(&other, "scope-b-1", "scope-beta", ab, mb, tb, 100, 200, 34);
        seed_null_scope(rt, &fixture, "scope-null-a", "scope-alpha", aa, ma, ta);
        seed_null_scope(rt, &fixture, "scope-null-b", "scope-beta", ab, mb, tb);
        seed_null_scope(
            rt,
            &fixture,
            "scope-null-legacy",
            "scope-legacy",
            "legacy-agent",
            "legacy-model",
            "legacy-tool",
        );
        let server = fixture.server(false);
        let get = |path: &str| get_ok(&server, path);
        let rows_a = get("/api/metrics/activity?workspace=ws_http_fixture");
        let rows_a = rows_a.as_array().unwrap();
        len_eq(rows_a, 2, "selected workspace sees only its groups");
        assert_no_activity(rows_a, "scope-beta", "other workspace");
        assert_no_activity(rows_a, "scope-legacy", "unscoped legacy");
        assert_fields!(rows_a[0], "activity_id": json!("scope-alpha"), "agent": json!("agent-a"), "model": json!("model-a"), "invocation_count": json!(2), "total_input_tokens": json!(20), "total_output_tokens": json!(40), "total_tokens": json!(60), "p50_tokens": json!(30), "p95_tokens": json!(30), "total_tool_calls": json!(2));
        assert_avg(&rows_a[0], "avg_tokens", 30.0);
        assert_fields!(rows_a[1], "activity_id": json!("scope-alpha-low"), "invocation_count": json!(1), "total_input_tokens": json!(1), "total_output_tokens": json!(1), "total_tokens": json!(2), "total_tool_calls": json!(1));
        let rows_b = get("/api/metrics/activity?workspace=ws_http_scope_other");
        let rows_b = rows_b.as_array().unwrap();
        len_eq(rows_b, 1, "other workspace sees only its group");
        assert_fields!(rows_b[0], "activity_id": json!("scope-beta"), "agent": json!("agent-b"), "model": json!("model-b"), "invocation_count": json!(1), "total_input_tokens": json!(100), "total_output_tokens": json!(200), "total_tokens": json!(300), "p50_tokens": json!(300), "p95_tokens": json!(300), "total_tool_calls": json!(1));
        assert_avg(&rows_b[0], "avg_tokens", 300.0);
        let tool_rows_a = get("/api/metrics/tools?workspace=ws_http_fixture");
        let tool_rows_a = tool_rows_a.as_array().unwrap();
        len_eq(
            tool_rows_a,
            2,
            "selected workspace sees only its tool groups",
        );
        assert_no_activity(tool_rows_a, "scope-beta", "other workspace");
        assert_no_activity(tool_rows_a, "scope-legacy", "unscoped legacy");
        assert_fields!(tool_rows_a[0], "activity_id": json!("scope-alpha"), "tool_name": json!("scope-tool-a"), "call_count": json!(2), "total_result_bytes": json!(24));
        assert_avg(&tool_rows_a[0], "avg_result_bytes", 12.0);
        assert_fields!(tool_rows_a[1], "activity_id": json!("scope-alpha-low"), "call_count": json!(1), "total_result_bytes": json!(12));
        let tool_rows_b = get("/api/metrics/tools?workspace=ws_http_scope_other");
        let tool_rows_b = tool_rows_b.as_array().unwrap();
        len_eq(tool_rows_b, 1, "other workspace sees only its tool group");
        assert_fields!(tool_rows_b[0], "activity_id": json!("scope-beta"), "tool_name": json!("scope-tool-b"), "call_count": json!(1), "total_result_bytes": json!(34));
        assert_avg(&tool_rows_b[0], "avg_result_bytes", 34.0);
        for (workspace, expected) in [
            (
                "ws_http_fixture",
                ["scope-a-1", "scope-a-2", "scope-a-low"].as_slice(),
            ),
            ("ws_http_scope_other", ["scope-b-1"].as_slice()),
        ] {
            let inv = "/api/metrics/invocations?limit=20&workspace=";
            let detail = get(&format!("{inv}{workspace}"));
            let runs: std::collections::BTreeSet<String> = detail
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["job_run_id"].as_str().unwrap().to_string())
                .collect();
            check_eq!(runs => expected.iter().map(|run| run.to_string()).collect(); "detailed listing stays workspace-isolated for {workspace}: {runs:?}");
        }
    }
);
