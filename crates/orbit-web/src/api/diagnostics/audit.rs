//! Shared v2 audit lookup and blob storage.

use orbit_common::storage::blob_store::BlobStore;
use orbit_core::{OrbitRuntime, V2AuditEventFilter};
use serde_json::Value;
use std::collections::HashMap;

pub(super) fn v2_audit_values(
    runtime: &OrbitRuntime,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    limit: usize,
) -> Result<Vec<Value>, orbit_core::OrbitError> {
    let rows = OrbitRuntime::list_v2_audit_events(
        runtime,
        V2AuditEventFilter {
            workspace_id: String::new(),
            since,
            until,
            source: Some("v2_envelope".to_string()),
            limit: Some(limit),
            ..Default::default()
        },
    )?;
    Ok(rows
        .into_iter()
        .rev()
        .filter_map(|row| serde_json::from_str::<Value>(&row.payload_json).ok())
        .collect())
}

pub(super) fn events_by_id(events: &[Value]) -> HashMap<&str, &Value> {
    events
        .iter()
        .filter_map(|event| {
            event
                .get("event_id")
                .and_then(Value::as_str)
                .map(|event_id| (event_id, event))
        })
        .collect()
}

pub(super) fn audit_blob_store(runtime: &OrbitRuntime) -> BlobStore {
    BlobStore::new(
        runtime
            .data_root()
            .join("state")
            .join("audit")
            .join("blobs"),
    )
}
