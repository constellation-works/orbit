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

#[test]
fn audit_event_with_json_keyed_credentials_is_stored_redacted_and_valid() {
    let store = Store::open_in_memory().unwrap();
    let payload = r#"{"event_id":"keyed","body_kind":"step_failed","headers":{"authorization":"Basic dXNlcjpwYXNz"},"x-api-key":"bare-xapi-key-value-01","api_key":"12345678901234567890","my_api_key":"prefixed-api-key-value1","serviceApiKey":"camel-api-key-value-001","note":"visible-value","api_keyboard":"ordinarybuildvalue"}"#;
    store
        .insert_v2_audit_event(&V2AuditEventInsertParams {
            workspace_id: "ws".into(),
            event_id: "keyed-row".into(),
            source: "v2_envelope".into(),
            schema_version: 1,
            event_type: "step_failed".into(),
            ts: chrono::Utc::now(),
            run_id: "keyed".into(),
            agent_identity: "agent".into(),
            parent_event_id: None,
            workspace_path: None,
            payload_json: payload.into(),
        })
        .unwrap();
    let rows = store
        .list_v2_audit_events(&V2AuditEventFilter {
            workspace_id: "ws".into(),
            run_id: Some("keyed".into()),
            source: Some("v2_envelope".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rows.len(), 1);
    let stored = &rows[0].payload_json;
    for secret in [
        "dXNlcjpwYXNz",
        "bare-xapi-key-value-01",
        "12345678901234567890",
        "prefixed-api-key-value1",
        "camel-api-key-value-001",
    ] {
        assert!(!stored.contains(secret), "secret leaked: {stored}");
    }
    let parsed: serde_json::Value = serde_json::from_str(stored)
        .unwrap_or_else(|err| panic!("stored payload is not valid JSON: {err}: {stored}"));
    assert_eq!(parsed["headers"]["authorization"], "[REDACTED_AUTH]");
    assert_eq!(parsed["x-api-key"], "[REDACTED_AUTH]");
    assert_eq!(parsed["api_key"], "[REDACTED_AUTH]");
    assert_eq!(parsed["my_api_key"], "[REDACTED_AUTH]");
    assert_eq!(parsed["serviceApiKey"], "[REDACTED_AUTH]");
    assert_eq!(parsed["note"], "visible-value");
    assert_eq!(parsed["api_keyboard"], "ordinarybuildvalue");
}

#[test]
fn audit_event_with_url_query_key_stays_valid_json_and_hides_the_key() {
    let store = Store::open_in_memory().unwrap();
    // Raw secret, and the already-redacted form a step produces before the
    // event is serialized: both place the URL at the end of the message, so
    // a whole-payload regex pass would consume the JSON terminators.
    let cases = [
        (
            "raw",
            "url-secret-123",
            r#"{"event_id":"raw","body_kind":"step_failed","message":"request failed https://example.test/?key=url-secret-123"}"#,
        ),
        (
            "pre-redacted",
            "[REDACTED_AUTH]",
            r#"{"event_id":"pre","body_kind":"step_failed","message":"request failed https://example.test/?key=[REDACTED_AUTH]"}"#,
        ),
    ];
    for (index, (run, _secret, payload)) in cases.iter().enumerate() {
        store
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: "ws".into(),
                event_id: format!("url-row-{index}"),
                source: "v2_envelope".into(),
                schema_version: 1,
                event_type: "step_failed".into(),
                ts: chrono::Utc::now(),
                run_id: (*run).into(),
                agent_identity: "agent".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: (*payload).into(),
            })
            .unwrap();
    }
    for (run, secret, _payload) in &cases {
        let rows = store
            .list_v2_audit_events(&V2AuditEventFilter {
                workspace_id: "ws".into(),
                run_id: Some((*run).into()),
                source: Some("v2_envelope".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(rows.len(), 1, "one stored row for {run}");
        let stored = &rows[0].payload_json;
        assert!(
            !stored.contains(secret) || *secret == "[REDACTED_AUTH]",
            "secret leaked for {run}: {stored}"
        );
        assert!(
            !stored.contains("url-secret-123"),
            "raw secret leaked for {run}: {stored}"
        );
        // The snapshot path (`serde_json::from_str` per row) must succeed.
        let parsed: serde_json::Value = serde_json::from_str(stored).unwrap_or_else(|err| {
            panic!("stored payload is not valid JSON for {run}: {err}: {stored}")
        });
        assert_eq!(
            parsed["body_kind"], "step_failed",
            "structure kept for {run}"
        );
        let message = parsed["message"].as_str().unwrap();
        assert!(
            message.contains("?key=[REDACTED_AUTH]"),
            "key hidden for {run}: {message}"
        );
        assert!(
            message.starts_with("request failed https://example.test/"),
            "message kept for {run}: {message}"
        );
    }
}
