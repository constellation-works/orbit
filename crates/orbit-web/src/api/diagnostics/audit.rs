//! Shared v2 audit lookup and blob storage.

use orbit_common::storage::blob_store::BlobStore;
use orbit_core::app::run::Command as RunCommand;
use orbit_core::{Core, V2AuditEventFilter};

use crate::state::run_read;

/// Payloads of up to `limit` of the newest v2 envelope events in
/// `[since, until]`, oldest first.
pub(super) fn v2_audit_payloads(
    runtime: &Core,
    since: Option<chrono::DateTime<chrono::Utc>>,
    until: Option<chrono::DateTime<chrono::Utc>>,
    limit: usize,
) -> Result<Vec<String>, orbit_core::OrbitError> {
    let rows = run_read(
        runtime,
        RunCommand::audit_events(V2AuditEventFilter {
            workspace_id: String::new(),
            since,
            until,
            source: Some("v2_envelope".to_string()),
            limit: Some(limit),
            ..Default::default()
        }),
    )?;
    Ok(rows.into_iter().rev().map(|row| row.payload_json).collect())
}

pub(super) fn audit_blob_store(runtime: &Core) -> Result<BlobStore, orbit_core::OrbitError> {
    run_read(runtime, RunCommand::audit_blob_root()).map(BlobStore::new)
}
