use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::{AuditEventFilter, Store};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpTransport, ToolSessionContext};
use serde_json::json;

use crate::adapter::command::dispatch::{
    ToolEntryPoint, execute_global_in_process_tool_dispatch, take_tool_audit_recorded,
};

use super::support::env_guard;

#[test]
fn persists_one_row_with_mcp_context() {
    let _guard = env_guard();
    let root = tempfile::tempdir().expect("global root");
    let _ = take_tool_audit_recorded();
    let context = ToolSessionContext {
        caller_machine_id: Some("hm_caller".to_string()),
        process_machine_id: Some("hm_server".to_string()),
        process_machine_name: Some("server-host".to_string()),
        transport: Some(McpTransport::SshMcp),
        trace_id: Some("trace-global".to_string()),
        caller_ip: Some("192.0.2.8".to_string()),
        ..ToolSessionContext::default()
    };

    let outcome = execute_global_in_process_tool_dispatch(
        root.path(),
        "orbit.workspace.list",
        json!({}),
        ToolEntryPoint::Mcp,
        context,
        |_| Ok(json!({ "workspaces": [] })),
    )
    .expect("global dispatch succeeds");
    assert!(outcome.audit_recorded);
    assert_eq!(outcome.value, json!({ "workspaces": [] }));

    let store = Store::open(&root.path().join("orbit.db")).expect("open global audit store");
    let rows = store
        .list_audit_events(&AuditEventFilter {
            tool_name: Some("orbit.workspace.list".to_string()),
            limit: 10,
            offset: 0,
            ..AuditEventFilter::default()
        })
        .expect("list global dispatch audit");
    assert_eq!(rows.len(), 1, "global call records exactly one row");
    let row = &rows[0];
    assert_eq!(row.status, AuditEventStatus::Success);
    assert_eq!(row.subcommand.as_deref(), Some("run-mcp"));
    assert_eq!(row.workspace_id, None);
    assert_eq!(row.caller_machine_id.as_deref(), Some("hm_caller"));
    assert_eq!(row.process_machine_id.as_deref(), Some("hm_server"));
    assert_eq!(row.process_machine_name.as_deref(), Some("server-host"));
    assert_eq!(row.transport, Some(McpTransport::SshMcp));
    assert_eq!(row.trace_id.as_deref(), Some("trace-global"));
    assert_eq!(row.caller_ip.as_deref(), Some("192.0.2.8"));
}

#[test]
fn audits_projection_failure_once() {
    let _guard = env_guard();
    let root = tempfile::tempdir().expect("global root");
    let result = execute_global_in_process_tool_dispatch(
        root.path(),
        "orbit.workspace.list",
        json!({}),
        ToolEntryPoint::Mcp,
        ToolSessionContext::default(),
        |_| {
            Err(OrbitError::InvalidInput(
                "workspace registry unavailable".to_string(),
            ))
        },
    );
    assert!(
        matches!(result, Err(OrbitError::InvalidInput(message)) if message == "workspace registry unavailable")
    );

    let store = Store::open(&root.path().join("orbit.db")).expect("open global audit store");
    let rows = store
        .list_audit_events(&AuditEventFilter {
            tool_name: Some("orbit.workspace.list".to_string()),
            limit: 10,
            offset: 0,
            ..AuditEventFilter::default()
        })
        .expect("list failed global dispatch audit");
    assert_eq!(rows.len(), 1, "failed global call records exactly one row");
    assert_eq!(rows[0].status, AuditEventStatus::Failure);
    assert_eq!(rows[0].subcommand.as_deref(), Some("run-mcp"));
}

#[test]
fn audits_unknown_mcp_tool_as_denied_once() {
    let _guard = env_guard();
    let root = tempfile::tempdir().expect("global root");
    let context = ToolSessionContext {
        caller_machine_id: Some("hm_caller".to_string()),
        process_machine_id: Some("hm_server".to_string()),
        process_machine_name: Some("server-host".to_string()),
        transport: Some(McpTransport::SshMcp),
        trace_id: Some("trace-unknown".to_string()),
        origin_session_id: Some("mcp-session-unknown".to_string()),
        ..ToolSessionContext::default()
    };

    let result = execute_global_in_process_tool_dispatch(
        root.path(),
        "orbit_unknown_raw",
        json!({}),
        ToolEntryPoint::Mcp,
        context,
        |_| {
            Err(OrbitError::not_found(
                NotFoundKind::Tool,
                "orbit_unknown_raw".to_string(),
            ))
        },
    );
    assert!(matches!(
        result,
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Tool,
            ..
        })
    ));

    let store = Store::open(&root.path().join("orbit.db")).expect("open global audit store");
    let rows = store
        .list_audit_events(&AuditEventFilter {
            tool_name: Some("orbit_unknown_raw".to_string()),
            limit: 10,
            offset: 0,
            ..AuditEventFilter::default()
        })
        .expect("list unknown-tool audit");
    assert_eq!(rows.len(), 1, "unknown call records exactly one row");
    let row = &rows[0];
    assert_eq!(row.status, AuditEventStatus::Denied);
    assert_eq!(row.subcommand.as_deref(), Some("run-mcp"));
    assert_eq!(row.workspace_id, None);
    assert_eq!(row.caller_machine_id.as_deref(), Some("hm_caller"));
    assert_eq!(row.process_machine_id.as_deref(), Some("hm_server"));
    assert_eq!(row.process_machine_name.as_deref(), Some("server-host"));
    assert_eq!(row.transport, Some(McpTransport::SshMcp));
    assert_eq!(row.trace_id.as_deref(), Some("trace-unknown"));
    assert_eq!(
        row.origin_session_id.as_deref(),
        Some("mcp-session-unknown")
    );
}

#[test]
fn fails_success_when_audit_store_cannot_open() {
    let _guard = env_guard();
    let root = tempfile::tempdir().expect("global root");
    std::fs::create_dir(root.path().join("orbit.db")).expect("block audit database path");
    let callback_ran = std::cell::Cell::new(false);

    let result = execute_global_in_process_tool_dispatch(
        root.path(),
        "orbit.workspace.list",
        json!({}),
        ToolEntryPoint::Mcp,
        ToolSessionContext::default(),
        |_| {
            callback_ran.set(true);
            Ok(json!({ "workspaces": [] }))
        },
    );

    assert!(callback_ran.get(), "server-local projection ran");
    assert!(
        matches!(result, Err(OrbitError::Store(_))),
        "successful projection must fail closed when audit persistence fails: {result:?}"
    );
}
