use crate::Store;
use orbit_types::tool::{McpCapability, McpTransport};
use std::collections::BTreeSet;

use super::sample_params;
use crate::AuditEventFilter;
use crate::AuditInvocationFields;
use crate::contracts::AuditEventInsertParams;

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

/// A plugin call's row names the secrets its request carried and nothing
/// more; every other row reads back with none, and nothing is written for it.
#[test]
fn plugin_secret_names_round_trip_and_default_to_none() {
    let store = Store::open_in_memory().expect("open store");
    let delivered = vec!["api_key".to_string(), "refresh_token".to_string()];
    store
        .insert_audit_event_record_with_invocation(
            &sample_params(),
            AuditInvocationFields {
                plugin_secrets: &delivered,
                ..AuditInvocationFields::default()
            },
        )
        .expect("insert plugin row");
    store
        .insert_audit_event_record_with_invocation(
            &AuditEventInsertParams {
                execution_id: "exec-test-plain".to_string(),
                ..sample_params()
            },
            AuditInvocationFields::default(),
        )
        .expect("insert plain row");

    let mut events = store
        .list_audit_events(&AuditEventFilter::default())
        .expect("list audit events");
    events.sort_by_key(|event| event.id);
    assert_eq!(events[0].plugin_secrets, delivered);
    assert!(events[1].plugin_secrets.is_empty());
    assert!(
        serde_json::to_value(&events[1])
            .expect("serialize")
            .get("plugin_secrets")
            .is_none(),
        "a row that delivered nothing does not grow the field"
    );
    let stored: Option<String> = store
        .conn
        .lock()
        .expect("conn")
        .query_row(
            "SELECT plugin_secrets FROM audit_events WHERE id = ?1",
            [events[1].id],
            |row| row.get(0),
        )
        .expect("read column");
    assert!(stored.is_none());
}

/// A plugin call's row keeps what its backend's `secret_updates` came to —
/// each name, applied or refused — and a row that rotated nothing writes
/// nothing and reads back empty.
#[test]
fn plugin_secret_update_outcomes_round_trip_and_default_to_none() {
    use orbit_types::plugin::PluginSecretUpdateStatus;
    use std::collections::BTreeMap;

    let store = Store::open_in_memory().expect("open store");
    let outcomes = BTreeMap::from([
        ("api_key".to_string(), PluginSecretUpdateStatus::Refused),
        (
            "refresh_token".to_string(),
            PluginSecretUpdateStatus::Applied,
        ),
    ]);
    store
        .insert_audit_event_record_with_invocation(
            &sample_params(),
            AuditInvocationFields {
                plugin_secret_updates: Some(&outcomes),
                ..AuditInvocationFields::default()
            },
        )
        .expect("insert rotating row");
    let nothing = BTreeMap::new();
    store
        .insert_audit_event_record_with_invocation(
            &AuditEventInsertParams {
                execution_id: "exec-test-plain".to_string(),
                ..sample_params()
            },
            AuditInvocationFields {
                plugin_secret_updates: Some(&nothing),
                ..AuditInvocationFields::default()
            },
        )
        .expect("insert plain row");

    let mut events = store
        .list_audit_events(&AuditEventFilter::default())
        .expect("list audit events");
    events.sort_by_key(|event| event.id);
    assert_eq!(events[0].plugin_secret_updates, outcomes);
    assert!(events[1].plugin_secret_updates.is_empty());
    let stored: Vec<Option<String>> = [events[0].id, events[1].id]
        .into_iter()
        .map(|id| {
            store
                .conn
                .lock()
                .expect("conn")
                .query_row(
                    "SELECT plugin_secret_updates FROM audit_events WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .expect("read column")
        })
        .collect();
    assert_eq!(
        stored,
        vec![
            Some(r#"{"api_key":"refused","refresh_token":"applied"}"#.to_string()),
            None
        ]
    );
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
