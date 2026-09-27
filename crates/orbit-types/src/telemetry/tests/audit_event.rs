#[cfg(any())]
mod execution_id {
    use super::super::audit_event::*;
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn audit_execution_id_is_unique_under_concurrent_generation() {
        let workers = 16;
        let per_worker = 64;
        let barrier = Arc::new(Barrier::new(workers));

        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    (0..per_worker)
                        .map(|_| audit_execution_id("exec"))
                        .collect::<Vec<_>>()
                })
            })
            .collect();

        let ids: Vec<String> = handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worker thread joined"))
            .collect();
        let unique: BTreeSet<_> = ids.iter().cloned().collect();

        assert_eq!(ids.len(), workers * per_worker);
        assert_eq!(unique.len(), ids.len());
        assert!(ids.iter().all(|id| id.starts_with("exec-")));
    }
}

mod tool_session_context {
    use std::collections::BTreeSet;

    use crate::tool::{McpCapability, McpTransport, ToolSessionContext};

    #[test]
    fn legacy_workspace_only_json_reads_with_default_additions() {
        let context: ToolSessionContext =
            serde_json::from_str(r#"{"workspace":"/repo"}"#).expect("legacy context");

        assert_eq!(context.workspace.as_deref(), Some("/repo"));
        assert_eq!(context.workspace_id, None);
        assert_eq!(context.transport, None);
        assert_eq!(context.trace_id, None);
        assert_eq!(context.caller_ip, None);
        assert!(context.effective_capabilities.is_empty());
        assert_eq!(context.origin_session_id, None);
        assert_eq!(context.mcp_call_id, None);
    }

    #[test]
    fn trusted_context_serializes_complete_sorted_capability_set() {
        let mut context = ToolSessionContext::trusted_local(
            Some("ws_orbit".to_string()),
            Some("hm_local".to_string()),
            Some("dk-local".to_string()),
        );
        context.effective_capabilities =
            BTreeSet::from([McpCapability::Agent, McpCapability::Operator]);
        context.origin_session_id = Some("mcp-session-1".to_string());
        context.mcp_call_id = Some("mcall-1".to_string());
        context.trace_id = Some("trace-1".to_string());
        context.caller_ip = Some("192.0.2.10".to_string());

        let value = serde_json::to_value(context).expect("serialize context");
        assert_eq!(value["transport"], "local");
        assert_eq!(value["trace_id"], "trace-1");
        assert_eq!(value["caller_ip"], "192.0.2.10");
        assert_eq!(
            value["effective_capabilities"],
            serde_json::json!(["agent", "operator"])
        );
    }

    #[test]
    fn transport_and_capability_parsers_are_typed() {
        assert_eq!("ssh-mcp".parse(), Ok(McpTransport::SshMcp));
        assert_eq!("runner".parse(), Ok(McpCapability::Runner));
        assert!("remote".parse::<McpTransport>().is_err());
        assert!("admin".parse::<McpCapability>().is_err());
    }
}

mod brokered_fields {
    use crate::telemetry::AuditEvent;

    /// A row serialized before the broker ran calls: no `brokered`, no
    /// `peer_pid`.
    const LEGACY_EVENT: &str = r#"{
        "id": 7,
        "execution_id": "exec-legacy",
        "timestamp": "2026-09-01T00:00:00Z",
        "command": "tool",
        "subcommand": "run",
        "tool_name": "demo.hello",
        "role": "claude",
        "status": "success",
        "exit_code": 0,
        "duration_ms": 3,
        "working_directory": "/repo",
        "pid": 4242
    }"#;

    #[test]
    fn a_legacy_payload_reads_as_not_brokered() {
        let event: AuditEvent = serde_json::from_str(LEGACY_EVENT).expect("legacy payload");

        assert!(!event.brokered);
        assert_eq!(event.peer_pid, None);
    }

    #[test]
    fn brokered_fields_round_trip_and_stay_off_other_rows() {
        let mut event: AuditEvent = serde_json::from_str(LEGACY_EVENT).expect("legacy payload");
        let plain = serde_json::to_value(&event).expect("serialize plain row");
        assert!(plain.get("brokered").is_none() && plain.get("peer_pid").is_none());

        event.brokered = true;
        event.peer_pid = Some(31337);
        let brokered = serde_json::to_value(&event).expect("serialize brokered row");
        assert_eq!(brokered["brokered"], true);
        assert_eq!(brokered["peer_pid"], 31337);
        let read: AuditEvent = serde_json::from_value(brokered).expect("read brokered row");
        assert_eq!(read, event);
    }
}
