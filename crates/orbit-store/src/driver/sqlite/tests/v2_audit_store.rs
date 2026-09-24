use chrono::Utc;

use crate::Store;
use crate::contracts::{V2AuditEventFilter, V2AuditEventInsertParams};

#[test]
fn insert_list_and_count_are_workspace_scoped() {
    let store = Store::open_in_memory().expect("store");
    let ts = Utc::now();
    for workspace_id in ["ws_a", "ws_b"] {
        store
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: workspace_id.to_string(),
                event_id: format!("evt-{workspace_id}"),
                source: "v2_envelope".to_string(),
                schema_version: 1,
                event_type: "tool.denied".to_string(),
                ts,
                run_id: "run-1".to_string(),
                agent_identity: "codex".to_string(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: "{}".to_string(),
            })
            .expect("insert");
    }

    let filter = V2AuditEventFilter {
        workspace_id: "ws_a".to_string(),
        event_type: Some("tool.denied".to_string()),
        ..Default::default()
    };
    let rows = store.list_v2_audit_events(&filter).expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event_id, "evt-ws_a");
    assert_eq!(store.count_v2_audit_events(&filter).expect("count"), 1);
}

/// [ORB-11625] Per-run partition limits keep a busy earlier run from
/// starving a later one. Presence ignores malformed payloads.
#[test]
fn partitioned_recovery_reads_do_not_starve_later_runs() {
    let store = Store::open_in_memory().expect("store");
    let busy = "jrun-busy";
    let quiet = "jrun-quiet";
    let empty = "jrun-empty";
    for index in 0..12_u32 {
        insert_envelope(
            &store,
            busy,
            &format!("evt-busy-{index}"),
            index,
            Some("step_recovery_attempted"),
        );
    }
    insert_envelope(&store, quiet, "evt-quiet-0", 0, Some("step_started"));
    insert_envelope(
        &store,
        quiet,
        "evt-quiet-1",
        1,
        Some("step_recovery_attempted"),
    );
    store
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: "ws_a".to_string(),
            event_id: "evt-malformed".to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "test.event".to_string(),
            ts: Utc::now(),
            run_id: empty.to_string(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: "{not-json".to_string(),
        })
        .expect("insert malformed");

    let run_ids = vec![busy.to_string(), quiet.to_string(), empty.to_string()];
    let rows = store
        .list_v2_audit_events_for_runs_partitioned(
            "ws_a",
            &run_ids,
            Some("v2_envelope"),
            Some("step_recovery_attempted"),
            9,
        )
        .expect("partitioned list");
    let busy_rows = rows
        .iter()
        .filter(|row| row.run_id == busy)
        .collect::<Vec<_>>();
    let quiet_rows = rows
        .iter()
        .filter(|row| row.run_id == quiet)
        .collect::<Vec<_>>();
    assert_eq!(busy_rows.len(), 9);
    assert_eq!(quiet_rows.len(), 1);
    assert_eq!(quiet_rows[0].event_id, "evt-quiet-1");
    assert!(!rows.iter().any(|row| row.run_id == empty));

    let present = store
        .list_v2_audit_run_ids_with_events("ws_a", &run_ids, Some("v2_envelope"))
        .expect("presence");
    assert!(present.contains(busy));
    assert!(present.contains(quiet));
    assert!(!present.contains(empty));
}

fn insert_envelope(
    store: &Store,
    run_id: &str,
    event_id: &str,
    minute: u32,
    body_kind: Option<&str>,
) {
    let ts = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 5, 1, 0, minute, 0)
        .single()
        .expect("ts");
    let payload = serde_json::json!({
        "event_id": event_id,
        "body_kind": body_kind,
        "run_id": run_id,
    });
    store
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: "ws_a".to_string(),
            event_id: event_id.to_string(),
            source: "v2_envelope".to_string(),
            schema_version: 1,
            event_type: "test.event".to_string(),
            ts,
            run_id: run_id.to_string(),
            agent_identity: "codex".to_string(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: payload.to_string(),
        })
        .expect("insert envelope");
}

/// A run's event pages read forward from its first event: `oldest_first`
/// windows must not start mid-run the way reversing a newest-first window does.
#[test]
fn oldest_first_windows_page_forward_from_the_first_event() {
    let store = Store::open_in_memory().expect("store");
    for minute in 0..5_u32 {
        insert_envelope(&store, "jrun-1", &format!("evt-{minute}"), minute, None);
    }
    let page = |oldest_first, offset| {
        store
            .list_v2_audit_events(&V2AuditEventFilter {
                workspace_id: "ws_a".to_string(),
                run_id: Some("jrun-1".to_string()),
                limit: Some(2),
                offset: Some(offset),
                oldest_first,
                ..Default::default()
            })
            .expect("list")
            .into_iter()
            .map(|row| row.event_id)
            .collect::<Vec<_>>()
    };

    assert_eq!(page(true, 0), ["evt-0", "evt-1"]);
    assert!(page(true, usize::MAX).is_empty(), "a huge offset is past the end");
    assert_eq!(page(true, 3), ["evt-3", "evt-4"]);
    assert_eq!(page(false, 0), ["evt-4", "evt-3"]);
}
