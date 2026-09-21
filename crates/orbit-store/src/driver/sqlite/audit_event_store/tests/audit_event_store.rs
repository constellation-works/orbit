// Migrated from sqlite/audit_event_store.rs per ORB-00231
use super::super::*;
use crate::Store;
use orbit_common::test_fixtures::{TEST_CLAUDE_MODEL, TEST_CODEX_MODEL};
use orbit_types::tool::{McpCapability, McpTransport};
use std::collections::BTreeSet;

fn sample_params() -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: "exec-test-1".to_string(),
        command: "tool".to_string(),
        subcommand: Some("run".to_string()),
        tool_name: Some("orbit.task.show".to_string()),
        target_type: Some("tool".to_string()),
        target_id: Some("orbit.task.show".to_string()),
        role: TEST_CLAUDE_MODEL.to_string(),
        status: AuditEventStatus::Success,
        exit_code: 0,
        duration_ms: 42,
        working_directory: "/tmp".to_string(),
        arguments_json: None,
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: None,
        host: Some("test-host".to_string()),
        pid: 1234,
        session_id: Some("session-abc".to_string()),
        workspace_id: Some("ws_orbit".to_string()),
        caller_machine_id: Some("hm_caller".to_string()),
        caller_machine_name: Some("caller-host".to_string()),
        process_machine_id: Some("hm_process".to_string()),
        process_machine_name: Some("process-host".to_string()),
        transport: Some(McpTransport::SshMcp),
        effective_capabilities: BTreeSet::from([McpCapability::Agent, McpCapability::Runner]),
        origin_session_id: Some("mcp-session-abc".to_string()),
        mcp_call_id: Some("mcall-abc".to_string()),
        lease_id: Some("lease-abc".to_string()),
        task_id: Some("T20260428-7".to_string()),
        job_run_id: Some("jrun-xyz".to_string()),
        activity_id: Some("agent_implement".to_string()),
        step_index: Some(2),
    }
}

fn sample_params_with(
    execution_id: &str,
    role: &str,
    status: AuditEventStatus,
) -> AuditEventInsertParams {
    AuditEventInsertParams {
        execution_id: execution_id.to_string(),
        role: role.to_string(),
        status,
        ..sample_params()
    }
}

#[test]
fn insert_then_read_round_trips_correlation_fields() {
    let store = Store::open_in_memory().expect("open store");
    let params = sample_params();
    store
        .insert_audit_event_record_with_invocation(
            &params,
            AuditInvocationFields {
                trace_id: Some("trace-test-1"),
                caller_ip: Some("192.0.2.10"),
                ..AuditInvocationFields::default()
            },
        )
        .expect("insert audit event");

    let events = store
        .list_audit_events(&AuditEventFilter::default())
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.task_id.as_deref(), Some("T20260428-7"));
    assert_eq!(event.job_run_id.as_deref(), Some("jrun-xyz"));
    assert_eq!(event.activity_id.as_deref(), Some("agent_implement"));
    assert_eq!(event.step_index, Some(2));
    assert_eq!(event.workspace_id.as_deref(), Some("ws_orbit"));
    assert_eq!(event.caller_machine_id.as_deref(), Some("hm_caller"));
    assert_eq!(event.process_machine_id.as_deref(), Some("hm_process"));
    assert_eq!(event.transport, Some(McpTransport::SshMcp));
    assert_eq!(
        event.effective_capabilities,
        BTreeSet::from([McpCapability::Agent, McpCapability::Runner])
    );
    assert_eq!(event.origin_session_id.as_deref(), Some("mcp-session-abc"));
    assert_eq!(event.mcp_call_id.as_deref(), Some("mcall-abc"));
    assert_eq!(event.trace_id.as_deref(), Some("trace-test-1"));
    assert_eq!(event.caller_ip.as_deref(), Some("192.0.2.10"));
    assert_eq!(event.lease_id.as_deref(), Some("lease-abc"));

    let by_id = store
        .get_audit_event(event.id)
        .expect("get audit event")
        .expect("event present");
    assert_eq!(by_id.task_id.as_deref(), Some("T20260428-7"));
    assert_eq!(by_id.job_run_id.as_deref(), Some("jrun-xyz"));
    assert_eq!(by_id.activity_id.as_deref(), Some("agent_implement"));
    assert_eq!(by_id.step_index, Some(2));
    assert_eq!(by_id.workspace_id.as_deref(), Some("ws_orbit"));
    assert_eq!(by_id.mcp_call_id.as_deref(), Some("mcall-abc"));
    assert_eq!(by_id.trace_id.as_deref(), Some("trace-test-1"));
    assert_eq!(by_id.caller_ip.as_deref(), Some("192.0.2.10"));
}

#[test]
fn list_audit_events_filters_trusted_provenance_and_capability_membership() {
    let store = Store::open_in_memory().expect("open store");
    store
        .insert_audit_event_record(&sample_params())
        .expect("insert audit event");

    let filters = [
        AuditEventFilter {
            workspace_id: Some("ws_orbit".to_string()),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            caller_machine_id: Some("hm_caller".to_string()),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            process_machine_id: Some("hm_process".to_string()),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            transport: Some(McpTransport::SshMcp),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            capability: Some(McpCapability::Runner),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            origin_session_id: Some("mcp-session-abc".to_string()),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            mcp_call_id: Some("mcall-abc".to_string()),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            job_run_id: Some("jrun-xyz".to_string()),
            ..AuditEventFilter::default()
        },
        AuditEventFilter {
            lease_id: Some("lease-abc".to_string()),
            ..AuditEventFilter::default()
        },
    ];
    for filter in filters {
        assert_eq!(
            store
                .list_audit_events(&filter)
                .expect("filter audit events")
                .len(),
            1
        );
    }

    let absent = store
        .list_audit_events(&AuditEventFilter {
            capability: Some(McpCapability::Operator),
            ..AuditEventFilter::default()
        })
        .expect("filter absent capability");
    assert!(absent.is_empty());
}

#[test]
fn list_audit_events_filters_by_target_type_kind() {
    let store = Store::open_in_memory().expect("open store");
    let mut hook = sample_params();
    hook.execution_id = "exec-hook".to_string();
    hook.target_type = Some("hook_event".to_string());
    store
        .insert_audit_event_record(&hook)
        .expect("insert hook event");

    let mut tool = sample_params();
    tool.execution_id = "exec-tool".to_string();
    tool.target_type = Some("tool".to_string());
    store
        .insert_audit_event_record(&tool)
        .expect("insert tool event");

    let events = store
        .list_audit_events(&AuditEventFilter {
            target_type: Some("hook_event".to_string()),
            ..AuditEventFilter::default()
        })
        .expect("list audit events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].execution_id, "exec-hook");
}

#[test]
fn migration_adds_correlation_columns_to_legacy_table() {
    let conn = rusqlite::Connection::open_in_memory().expect("open in-memory connection");

    // Simulate a pre-migration audit_events table without correlation columns.
    conn.execute_batch(
        r#"
                CREATE TABLE audit_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    execution_id TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    command TEXT NOT NULL,
                    subcommand TEXT,
                    tool_name TEXT,
                    target_type TEXT,
                    target_id TEXT,
                    role TEXT NOT NULL,
                    status TEXT NOT NULL,
                    exit_code INTEGER NOT NULL,
                    duration_ms INTEGER NOT NULL,
                    working_directory TEXT NOT NULL,
                    arguments_json TEXT,
                    stdout_truncated TEXT,
                    stderr_truncated TEXT,
                    error_message TEXT,
                    host TEXT,
                    pid INTEGER NOT NULL,
                    session_id TEXT
                );
                INSERT INTO audit_events(
                    execution_id, timestamp, command, role, status, exit_code,
                    duration_ms, working_directory, pid
                ) VALUES (
                    'exec-legacy', '2026-04-28T00:00:00Z', 'tool', 'claude-opus-4-7',
                    'success', 0, 1, '/tmp', 1
                );
            "#,
    )
    .expect("seed legacy schema");

    crate::driver::sqlite::migration::apply_schema(&conn).expect("apply schema");

    let mut stmt = conn
        .prepare("PRAGMA table_info(audit_events)")
        .expect("prepare pragma");
    let columns: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .expect("query pragma")
        .collect::<Result<_, _>>()
        .expect("collect pragma rows");
    for expected in [
        "task_id",
        "job_run_id",
        "activity_id",
        "step_index",
        "workspace_id",
        "caller_machine_id",
        "caller_machine_name",
        "process_machine_id",
        "process_machine_name",
        "transport",
        "capabilities_json",
        "origin_session_id",
        "mcp_call_id",
        "trace_id",
        "caller_ip",
        "lease_id",
    ] {
        assert!(
            columns.iter().any(|c| c == expected),
            "expected column `{expected}` in {columns:?}"
        );
    }

    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND tbl_name='audit_events'")
        .expect("prepare index query");
    let indexes: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query indexes")
        .collect::<Result<_, _>>()
        .expect("collect index rows");
    assert!(indexes.iter().any(|i| i == "idx_audit_events_task_id"));
    assert!(indexes.iter().any(|i| i == "idx_audit_events_job_run_id"));
    assert!(indexes.iter().any(|i| i == "idx_audit_events_workspace_id"));
    assert!(indexes.iter().any(|i| i == "idx_audit_events_mcp_call_id"));
    assert!(indexes.iter().any(|i| i == "idx_audit_events_trace_id"));

    let preserved: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_events WHERE execution_id = 'exec-legacy'",
            [],
            |row| row.get(0),
        )
        .expect("count legacy rows");
    assert_eq!(preserved, 1, "migration must preserve existing rows");

    let task_id: Option<String> = conn
        .query_row(
            "SELECT task_id FROM audit_events WHERE execution_id = 'exec-legacy'",
            [],
            |row| row.get(0),
        )
        .expect("read legacy row task_id");
    assert!(
        task_id.is_none(),
        "legacy row should have NULL task_id post-migration",
    );
    struct LegacyInvocationFields {
        workspace_id: Option<String>,
        capabilities_json: Option<String>,
        mcp_call_id: Option<String>,
        trace_id: Option<String>,
        caller_ip: Option<String>,
    }
    let new_fields = conn
        .query_row(
            "SELECT workspace_id, capabilities_json, mcp_call_id, trace_id, caller_ip FROM audit_events \
             WHERE execution_id = 'exec-legacy'",
            [],
            |row| {
                Ok(LegacyInvocationFields {
                    workspace_id: row.get(0)?,
                    capabilities_json: row.get(1)?,
                    mcp_call_id: row.get(2)?,
                    trace_id: row.get(3)?,
                    caller_ip: row.get(4)?,
                })
            },
        )
        .expect("read legacy row additions");
    assert!(new_fields.workspace_id.is_none());
    assert!(new_fields.capabilities_json.is_none());
    assert!(new_fields.mcp_call_id.is_none());
    assert!(new_fields.trace_id.is_none());
    assert!(new_fields.caller_ip.is_none());
}

#[test]
fn tool_call_counts_by_role_include_failed_and_denied_runs() {
    let store = Store::open_in_memory().expect("open store");

    for params in [
        sample_params_with("exec-success", "codex / gpt-5", AuditEventStatus::Success),
        sample_params_with("exec-failure", "codex / gpt-5", AuditEventStatus::Failure),
        sample_params_with("exec-denied", "codex / gpt-5", AuditEventStatus::Denied),
    ] {
        store
            .insert_audit_event_record(&params)
            .expect("insert audit event");
    }

    let mut non_run = sample_params_with("exec-show", "codex / gpt-5", AuditEventStatus::Failure);
    non_run.subcommand = Some("show".to_string());
    store
        .insert_audit_event_record(&non_run)
        .expect("insert non-run audit event");

    let rows = store
        .get_audit_tool_call_counts_by_role(None)
        .expect("load tool call counts");

    assert_eq!(
        rows,
        vec![AuditToolCallCountsByRole {
            role: "codex / gpt-5".to_string(),
            total: 3,
            failed: 2,
        }]
    );
}

#[test]
fn tool_call_counts_by_surface_and_role_extract_segment_after_orbit_prefix() {
    let store = Store::open_in_memory().expect("open store");

    let mut adr_show = sample_params_with(
        "exec-adr-show-1",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Success,
    );
    adr_show.tool_name = Some("orbit.adr.show".to_string());
    adr_show.target_id = Some("orbit.adr.show".to_string());
    store.insert_audit_event_record(&adr_show).expect("insert");

    let mut adr_show_failed = sample_params_with(
        "exec-adr-show-2",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Failure,
    );
    adr_show_failed.tool_name = Some("orbit.adr.show".to_string());
    adr_show_failed.target_id = Some("orbit.adr.show".to_string());
    store
        .insert_audit_event_record(&adr_show_failed)
        .expect("insert");

    let mut config_show = sample_params_with(
        "exec-config-show",
        TEST_CODEX_MODEL,
        AuditEventStatus::Success,
    );
    config_show.tool_name = Some("orbit.config.show".to_string());
    config_show.target_id = Some("orbit.config.show".to_string());
    store
        .insert_audit_event_record(&config_show)
        .expect("insert");

    let mut task_update = sample_params_with(
        "exec-task-update",
        TEST_CODEX_MODEL,
        AuditEventStatus::Success,
    );
    task_update.tool_name = Some("orbit.task.update".to_string());
    task_update.target_id = Some("orbit.task.update".to_string());
    store
        .insert_audit_event_record(&task_update)
        .expect("insert");

    // Non-orbit tool name must be excluded.
    let mut external = sample_params_with(
        "exec-external",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Success,
    );
    external.tool_name = Some("github.create_pr".to_string());
    external.target_id = Some("github.create_pr".to_string());
    store.insert_audit_event_record(&external).expect("insert");

    // Non-`run`/`run-mcp` subcommand must be excluded even on an orbit name.
    let mut non_run = sample_params_with(
        "exec-show-noise",
        TEST_CLAUDE_MODEL,
        AuditEventStatus::Success,
    );
    non_run.subcommand = Some("show".to_string());
    non_run.tool_name = Some("orbit.adr.show".to_string());
    non_run.target_id = Some("orbit.adr.show".to_string());
    store.insert_audit_event_record(&non_run).expect("insert");

    let rows = store
        .get_audit_tool_call_counts_by_surface_and_role(None)
        .expect("surface counts");

    assert_eq!(
        rows,
        vec![
            AuditToolCallCountsBySurfaceAndRole {
                surface: "adr".to_string(),
                role: TEST_CLAUDE_MODEL.to_string(),
                total: 2,
                failed: 1,
            },
            AuditToolCallCountsBySurfaceAndRole {
                surface: "config".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 1,
                failed: 0,
            },
            AuditToolCallCountsBySurfaceAndRole {
                surface: "task".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 1,
                failed: 0,
            },
        ]
    );
}

#[test]
fn top_tool_calls_groups_by_tool_name_and_role_with_limit() {
    let store = Store::open_in_memory().expect("open store");

    // gpt-5.5: 3x orbit.task.show
    for i in 0..3 {
        let mut p = sample_params_with(
            &format!("exec-show-{i}"),
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("orbit.task.show".to_string());
        p.target_id = Some("orbit.task.show".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // claude-opus-4-7: 2x orbit.search
    for i in 0..2 {
        let mut p = sample_params_with(
            &format!("exec-claude-search-{i}"),
            TEST_CLAUDE_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("orbit.search".to_string());
        p.target_id = Some("orbit.search".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // gpt-5.5: 1× orbit.task.update
    {
        let mut p = sample_params_with(
            "exec-task-update",
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("orbit.task.update".to_string());
        p.target_id = Some("orbit.task.update".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // Non-orbit tool — must be excluded.
    {
        let mut p = sample_params_with(
            "exec-non-orbit",
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.tool_name = Some("github.create_pr".to_string());
        p.target_id = Some("github.create_pr".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    // Non-`run`/`run-mcp` subcommand on an orbit name — must be excluded.
    {
        let mut p = sample_params_with(
            "exec-show-noise",
            TEST_CODEX_MODEL,
            AuditEventStatus::Success,
        );
        p.subcommand = Some("show".to_string());
        p.tool_name = Some("orbit.task.show".to_string());
        p.target_id = Some("orbit.task.show".to_string());
        store.insert_audit_event_record(&p).expect("insert");
    }

    let rows = store
        .get_audit_top_tool_calls(None, 0)
        .expect("top tool calls");
    assert_eq!(
        rows,
        vec![
            AuditTopToolCall {
                tool_name: "orbit.task.show".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 3,
            },
            AuditTopToolCall {
                tool_name: "orbit.search".to_string(),
                role: TEST_CLAUDE_MODEL.to_string(),
                total: 2,
            },
            AuditTopToolCall {
                tool_name: "orbit.task.update".to_string(),
                role: TEST_CODEX_MODEL.to_string(),
                total: 1,
            },
        ]
    );

    // Limit caps the row count, preserving sort order.
    let limited = store
        .get_audit_top_tool_calls(None, 2)
        .expect("top tool calls limited");
    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].tool_name, "orbit.task.show");
    assert_eq!(limited[1].tool_name, "orbit.search");
}

#[test]
fn audit_event_aggregates_by_tool_splits_failures_by_surface() {
    let store = Store::open_in_memory().expect("open store");
    let since = chrono::Utc::now() - chrono::Duration::hours(1);

    let mut cli_ok = sample_params_with("exec-cli-ok", "codex", AuditEventStatus::Success);
    cli_ok.subcommand = Some("run".to_string());
    cli_ok.tool_name = Some("orbit.search".to_string());
    cli_ok.duration_ms = 50;
    store.insert_audit_event_record(&cli_ok).expect("insert");

    let mut cli_fail = sample_params_with("exec-cli-fail", "codex", AuditEventStatus::Failure);
    cli_fail.subcommand = Some("run".to_string());
    cli_fail.tool_name = Some("orbit.search".to_string());
    cli_fail.duration_ms = 150;
    store.insert_audit_event_record(&cli_fail).expect("insert");

    let mut mcp_fail = sample_params_with("exec-mcp-fail", "codex", AuditEventStatus::Failure);
    mcp_fail.subcommand = Some("run-mcp".to_string());
    mcp_fail.tool_name = Some("orbit.search".to_string());
    mcp_fail.duration_ms = 250;
    store.insert_audit_event_record(&mcp_fail).expect("insert");

    // Event with NULL tool_name folds into "unknown".
    let mut no_tool = sample_params_with("exec-no-tool", "codex", AuditEventStatus::Success);
    no_tool.subcommand = None;
    no_tool.tool_name = None;
    no_tool.duration_ms = 10;
    store.insert_audit_event_record(&no_tool).expect("insert");

    let rows = store
        .get_audit_event_aggregates_by_tool(&since)
        .expect("aggregates by tool");

    let search = rows
        .iter()
        .find(|r| r.tool_name == "orbit.search")
        .expect("orbit.search row");
    assert_eq!(search.total, 3);
    assert_eq!(search.successes, 1);
    assert_eq!(search.failures, 2);
    assert_eq!(search.denials, 0);
    assert_eq!(search.mcp_total, 1);
    assert_eq!(search.cli_total, 2);
    assert_eq!(search.mcp_failures, 1);
    assert_eq!(search.cli_failures, 1);
    assert_eq!(search.avg_duration_ms.round() as i64, 150);

    let unknown = rows
        .iter()
        .find(|r| r.tool_name == "unknown")
        .expect("unknown bucket");
    assert_eq!(unknown.total, 1);
    assert_eq!(unknown.successes, 1);
    assert_eq!(unknown.failures, 0);
    assert_eq!(unknown.denials, 0);
    assert_eq!(unknown.mcp_total, 0);
    assert_eq!(unknown.cli_total, 0);
}

#[test]
fn audit_event_aggregates_by_role_splits_subcommand_surface() {
    let store = Store::open_in_memory().expect("open store");
    let since = chrono::Utc::now() - chrono::Duration::hours(1);

    let mut codex_cli = sample_params_with("exec-codex-cli", "codex", AuditEventStatus::Success);
    codex_cli.subcommand = Some("run".to_string());
    store.insert_audit_event_record(&codex_cli).expect("insert");

    let mut codex_mcp = sample_params_with("exec-codex-mcp", "codex", AuditEventStatus::Success);
    codex_mcp.subcommand = Some("run-mcp".to_string());
    store.insert_audit_event_record(&codex_mcp).expect("insert");

    let mut codex_other =
        sample_params_with("exec-codex-other", "codex", AuditEventStatus::Success);
    codex_other.subcommand = Some("show".to_string());
    store
        .insert_audit_event_record(&codex_other)
        .expect("insert");

    let mut codex_internal =
        sample_params_with("exec-codex-internal", "codex", AuditEventStatus::Success);
    codex_internal.subcommand = None;
    store
        .insert_audit_event_record(&codex_internal)
        .expect("insert");

    let mut human = sample_params_with("exec-human", "human", AuditEventStatus::Success);
    human.subcommand = Some("run".to_string());
    store.insert_audit_event_record(&human).expect("insert");

    let rows = store
        .get_audit_event_aggregates_by_role(&since)
        .expect("aggregates by role");

    let codex = rows.iter().find(|r| r.role == "codex").expect("codex row");
    assert_eq!(codex.total, 4);
    assert_eq!(codex.mcp, 1);
    assert_eq!(codex.cli, 1);
    assert_eq!(codex.other, 1);
    assert_eq!(codex.no_subcommand, 1);
    assert_eq!(
        codex.mcp + codex.cli + codex.other + codex.no_subcommand,
        codex.total
    );

    let human = rows.iter().find(|r| r.role == "human").expect("human row");
    assert_eq!(human.total, 1);
    assert_eq!(human.mcp, 0);
    assert_eq!(human.cli, 1);
    assert_eq!(human.other, 0);
    assert_eq!(human.no_subcommand, 0);
    assert_eq!(
        human.mcp + human.cli + human.other + human.no_subcommand,
        human.total
    );
}
