use orbit_store::contracts::{V2AuditEventFilter, V2AuditEventInsertParams};

#[test]
fn audit_presence_and_partitioned_reads_require_string_envelope_ids() {
    let store = Store::open_in_memory().unwrap();
    let payloads = [
        ("number", r#"{"event_id":123,"body_kind":"step_started"}"#),
        ("boolean", r#"{"event_id":true,"body_kind":"step_started"}"#),
        ("array", r#"{"event_id":[],"body_kind":"step_started"}"#),
        ("object", r#"{"event_id":{},"body_kind":"step_started"}"#),
        ("null", r#"{"event_id":null,"body_kind":"step_started"}"#),
        ("missing", r#"{"body_kind":"step_started"}"#),
        ("malformed", "{"),
        ("valid", r#"{"event_id":"good","body_kind":"step_started"}"#),
        ("mixed", r#"{"event_id":false,"body_kind":"step_started"}"#),
        (
            "mixed",
            r#"{"event_id":"good-mixed","body_kind":"step_started"}"#,
        ),
    ];
    for (index, (run, payload)) in payloads.iter().enumerate() {
        store
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: "ws".into(),
                event_id: format!("row-{index}"),
                source: "v2_envelope".into(),
                schema_version: 1,
                event_type: "step_started".into(),
                ts: chrono::Utc::now(),
                run_id: (*run).into(),
                agent_identity: "agent".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: (*payload).into(),
            })
            .unwrap();
    }
    let runs = payloads
        .iter()
        .map(|(run, _)| (*run).to_string())
        .collect::<Vec<_>>();
    let present = store
        .list_v2_audit_run_ids_with_events("ws", &runs, Some("v2_envelope"))
        .unwrap();
    assert_eq!(present, HashSet::from(["valid".into(), "mixed".into()]));
    let rows = store
        .list_v2_audit_events_for_runs_partitioned(
            "ws",
            &runs,
            Some("v2_envelope"),
            Some("step_started"),
            10,
        )
        .unwrap();
    assert_eq!(rows.len(), 2);
    let filter = V2AuditEventFilter {
        workspace_id: "ws".into(),
        body_kinds: vec!["step_started".into()],
        ..Default::default()
    };
    assert_eq!(store.count_v2_audit_events(&filter).unwrap(), 2);
}
