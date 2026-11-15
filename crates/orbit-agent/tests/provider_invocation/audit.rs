use orbit_agent::loop_engine::{AuditSink, InMemorySink};

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
