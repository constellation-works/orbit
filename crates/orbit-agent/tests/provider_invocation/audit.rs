use orbit_agent::loop_engine::{AuditSink, InMemorySink, LoopAuditEvent};
use serde_json::Value;

use super::support::{isolated, scratch};

#[test]
fn sink_redacts_secret_before_storing_blob_bytes() {
    isolated(
        "audit::sink_redacts_secret_before_storing_blob_bytes",
        || {
            let fixture = scratch();
            let sink = InMemorySink::new(fixture.path());
            let secret = "secret-xyz";
            let payload = format!(
                "Authorization: Bearer {secret}\nx-api-key: {secret}\n{{\"api_key\":\"{secret}\"}}"
            );
            let hash = sink.write_blob(payload.as_bytes());
            let stored = sink.blob_store().read(&hash).expect("read stored blob");
            let text = String::from_utf8(stored).expect("text blob");
            assert!(
                !text.contains(secret),
                "sink must redact secrets at write time"
            );
            assert!(
                text.contains("[REDACTED_AUTH]"),
                "stored blob has redaction marker"
            );
        },
    );
}

#[test]
fn historical_loop_event_payloads_remain_readable_without_shape_changes() {
    // Frozen schema-v1 payloads from the persisted loop_event contract, covering
    // every retained variant, including the retired HTTP runtime's records.
    let rows: Vec<Value> = serde_json::from_str(include_str!("fixtures/loop_events.json"))
        .expect("historical event fixtures");
    for row in rows {
        let event: LoopAuditEvent = serde_json::from_str(&row.to_string())
            .expect("historical v2_audit_events payload remains deserializable");
        assert_eq!(
            serde_json::to_value(event).expect("serialize retained event"),
            row,
            "schema-v1 event_kind and payload fields must stay compatible"
        );
    }
}
