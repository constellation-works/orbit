use crate::Store;
use orbit_types::tool::{McpCapability, McpTransport};

use super::sample_params;
use crate::AuditEventFilter;

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
